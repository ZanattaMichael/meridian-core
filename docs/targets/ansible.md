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
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)) |
| `dependsOn` | Yes, as task order. A play runs top to bottom, so independent resources still run in sequence; a warning says which |
| `notifies` | Native: handlers. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec. The action is kept: a reload stays a reload |
| `runtimeWhen` | Yes: a task's `when:`, as an Ansible expression |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | Yes: `--check --diff`. A task Ansible skips in check mode cannot be previewed, and makes the plan unknown |
| Apply | Yes |
| Outputs | No. A play produces no values for a later stage to read |
| Import | Yes: playbooks and task files. See [Importing](#importing) |
| Compiles to | `playbook.yml` and `inventory.ini` |
| Runs with | `ansible-playbook`, over whatever connection Ansible is configured for |

What it does not do:

- **No roles, no collections, no vault.** A play is one list of tasks built
  from `ansible.builtin` modules. What the document says is all the play says.
- **No resource types beyond the six.** There is no passthrough to arbitrary
  modules; a `template`, `lineinfile` or `git` task has no resource type.
  Anything else is refused at compile time.
- **No inventory beyond the host.** `inventory.ini` holds the document's one
  host. Groups, host variables and connection settings go through
  [`MERIDIAN_ANSIBLE_ARGS`](#settings) or Ansible's own configuration.
- **`package.version` is refused**, though Ansible could express it; see
  [What is refused](#what-is-refused).
- **It creates no infrastructure.** It configures a host that already exists.

Contents: [Requirements](#requirements) ·
[A first document](#a-first-document) ·
[Resource types](#resource-types) ·
[Ordering and notifications](#ordering-and-notifications) ·
[What a compile writes](#what-a-compile-writes) ·
[Importing](#importing) ·
[Running](#running) ·
[Settings](#settings) ·
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

Ansible renders every string in a task through Jinja on the control machine,
so a Jinja expression written in the document, such as
`{{ ansible_facts['os_family'] }}`, is passed through and evaluated when the
play runs. A value Meridian places from `--data`, `--set`, a data layer or
`env()` is not: the plugin declares Jinja's openings (`{{`, `{%`, `{#`), and
the core writes each one in a placed value as an expression that yields itself
(`{{ '{{' }}`), which Jinja renders back to the text. So a data value holding
`{{ lookup('pipe', 'id') }}` reaches the host as that text, and never runs
`id` on the control machine
([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions)).

The host in `inventory.ini` is one line, so the plugin refuses a host that is
not a name (`invalid-host`); the core refuses one before it gets this far
([`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)).

## Importing

`meridian import site.yml` reads playbooks and task files and writes one ResourceSet for
each play. [Importing existing configuration](../operations/importing.md) is the guide. `site.yml`, `playbook.yml` and any name with `playbook` in it
are recognised; for another name say `--from ansible`.

A playbook says far more than the six resource types do, so this importer reports more
than any other. What it carries over is real and exact; what it does not is listed, not
approximated. In particular it never turns a task it does not understand into an `exec`.

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
| A task whose module has no resource type: `template`, `lineinfile`, `git`, `debug`, any other | Left out, with the module named | not translated |
| `copy` with `src`, and `file` with `state: link`, `hard` or `touch` | Left out. A document carries a file's content, not a path on the machine Ansible runs on | not translated |
| `roles`, `include_tasks`, `import_tasks`, `include_role`, `import_role`, `import_playbook` | Left out, naming what was brought in. Import that file as well | not translated |
| An argument the resource type has no param for, such as `update_cache` | The resource is imported without it, and the argument is named | not translated |
| A task keyword a resource cannot carry: `become`, `register`, `tags`, `environment`, `ignore_errors`, `changed_when`, `delegate_to` and the rest | The resource is imported without it, and the keyword is named | not translated |
| A play keyword: `become`, `remote_user`, `vars_files`, `serial`, `environment` and the rest | Left out. Connection settings belong in `MERIDIAN_ANSIBLE_ARGS` | not translated |
| A `loop` over a variable or an expression | The task is left out | not translated |
| A play variable that is a list, a mapping or a template, and any play variable used inside an expression such as `{{ port | int + 1 }}` or in a `when` | The resource that uses it is marked: Ansible will find the variable undefined | not translated |
| A `notify` whose handler is not a service action, acts on a service no task manages, or does not exist | The notification is left out, saying which | not translated |
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
| `MERIDIAN_ANSIBLE_ARGS` | arguments added to every run, split on spaces: `--user deploy --private-key ~/.ssh/deploy`, or `--connection local` for this machine |

## What is refused

Every refusal names the resource, the line and the rule. The rules this
target can raise, and what to do:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type outside the six, or in the wrong case (`Package`) | use one of the six; there is no passthrough |
| `unsupported-param` | a param this type does not take on Ansible, including `package.version` | drop it |
| `missing-param` | no `name`, `path` or `command` | add it |
| `empty-param` | a required param that is empty text | give it a value |
| `unsupported-state` | a state this type does not have, or any state on an `exec` | see the table under [Resource types](#resource-types) |
| `unsupported-action` | a notify action the type does not take: anything on a package, file, user or group | notify a service or an exec |
| `unknown-notify-target` | a notify names a resource the document does not declare | fix the id |
| `ordering-violated` | the order the core handed over puts a resource before its dependency | a core bug; report it |
| `duplicate-resource` | two resources with the same id | rename one |
| `duplicate-task-name`, `duplicate-handler-name` | two tasks or two handlers that would share a name | rename one; a handler is named after its action and resource |
| `empty-name`, `multiline-name` | an id that cannot be a task name | use one line of letters, digits and underscores |
| `missing-module`, `empty-module-args` | a task with no module, or a module given nothing | a plugin bug; report it |
| `missing-handler` | a task notifies a handler the play does not define | a plugin bug; report it |
| `conditional-handler` | a `runtimeWhen` on a handler | a plugin bug; report it |
| `dangling-reference` | `${id.param}` names a resource or a param that is not there | fix the reference |
| `invalid-host` | a host that is not a name, so it cannot be an inventory line | the core refuses one first ([`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)) |
| `missing-host` | a ResourceSet with no host to write the inventory for | give it a `targetHost` |

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| `could not run ansible-playbook: it is not installed, or not on the PATH` | the plugin runs `ansible-playbook` on the machine running Meridian | `pip install ansible-core`, or set `MERIDIAN_ANSIBLE_PLAYBOOK` |
| `ansible-playbook could not reach the host` (status `4`) | SSH, the user or the key is not what Ansible needs | put the connection settings in `MERIDIAN_ANSIBLE_ARGS`, or `--connection local` for this machine |
| the plan is unknown | a task was skipped in check mode: an exec with no `creates` or `removes`, or a condition only the host can answer | give the exec a `creates`, or read the apply's report |
| `ansible-playbook reported a failed task` (status `2`) | a task failed on the host; the ones before it were applied | read the output above the message; `apply --resume` continues after the fix |
| a handler did not run | handlers run only when a task that notifies them changed | expected; change the notifier, or declare the state you want on the service |
| a warning that resources are independent but run in sequence | a play is one sequence | expected; it says what the target cannot express |

## How it is tested

- **Golden files** under `plugins/ansible/testdata/` record what every fixture
  compiles to, byte for byte, the warnings it raises, and what each refusal
  says.
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
  check mode cannot preview, and a failed task. They are marked `#[ignore]`
  and run with
  `MERIDIAN_TEST_REAL_ANSIBLE=1 cargo test -p meridian-target-ansible --test real -- --ignored`.
  Reaching another machine over SSH, and the package, service, user and group
  modules, have only met stand-ins. See
  [Real-tool tests](../contributing/real-tool-tests.md).
