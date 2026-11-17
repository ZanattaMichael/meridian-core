# Target: `ansible`

<img src="../branding/logos/meridian-ansible.svg" width="72" alt="Meridian Ansible plugin logo">

Compiles a ResourceSet into a playbook and an inventory, and runs them with
`ansible-playbook`.

## What it does and does not support

| | |
|---|---|
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)) |
| `dependsOn` | Yes, as task order. Ansible runs a play top to bottom, so independent resources still run in sequence; a warning says which |
| `notifies` | Native: handlers. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec |
| `runtimeWhen` | Yes: a task's `when:` |
| Plan | Yes: `--check --diff`. A task Ansible skips in check mode cannot be previewed |
| Outputs | No |
| Import | Yes: playbooks and task files. See [Importing](#importing) |

## What a document compiles to

`playbook.yml` (one play, one task a resource, named by the resource's id) and
`inventory.ini` (the document's host). Modules are the `ansible.builtin` ones:
`package`, `service`, `copy` for a file with content and `file` otherwise,
`user`, `group`, `command`, and `shell` for an exec with `shell: true`.

A notify becomes a handler named after the action and the resource, such as
`reload nginx_svc`. Handlers run once, at the end of the play, only when a task
that notifies them changed something.

`runtimeWhen` is copied into the task's `when:` as written, so it is an Ansible
expression:

```yaml
runtimeWhen: "ansible_facts['os_family'] == 'Debian'"
```

## Example

[`examples/ansible/web-server.yaml`](../../examples/ansible/web-server.yaml), walked
through in [your first ResourceSet](../quickstart/first-resourceset.md).

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
| `MERIDIAN_ANSIBLE_PLAYBOOK` | the program to run, when `ansible-playbook` is not on `PATH` |
| `MERIDIAN_ANSIBLE_ARGS` | arguments added to every run: `--user deploy --private-key ~/.ssh/deploy`, or `--connection local` |

Changes are read from Ansible's own report (`changed:` per task and handler). A
failed task or an unreachable host fails the stage, with the end of Ansible's
output.

The runner is tested against a real `ansible-playbook` (ansible-core 2.19) on
every pull request that touches it, over the `local` connection: a check-mode
plan that creates nothing, a play checked on disk, a second plan that is in
sync, drift put right, a handler that runs only when its notifier changed, a
`runtimeWhen` answered by the host's facts, a command check mode cannot
preview, and a failed task. Reaching another machine over SSH, and the
package, service, user and group modules, have only met stand-ins. See
[Real-tool tests](../contributing/real-tool-tests.md).
