# Target: `ansible`

<img src="../branding/logos/meridian-ansible.svg" width="72" alt="Meridian Ansible plugin logo">

Compiles a ResourceSet into a playbook and an inventory, run with `ansible-playbook` from
the machine Meridian runs on.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)), and any module by its full name, such as `ansible.builtin.lineinfile` ([modules beside the six](#modules-beside-the-six)) |
| `dependsOn` | Yes, as task order. A play runs top to bottom, so independent resources still run in sequence; a warning says which |
| `notifies` | Native: handlers. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec or on a module. The action is kept: a reload stays a reload |
| `runtimeWhen` | Yes: a task's `when:`, as an Ansible expression |
| `adopt` | Accepted, and changes nothing: Ansible keeps no state and makes each thing what the document says whether it exists or not. The compile warns |
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

Not supported:

- **Roles and vault.** A play is one task list. Collections are not installed for you.
- **Short module names.** Write `ansible.builtin.lineinfile`.
- **Inventory beyond the host.** Use [`MERIDIAN_ANSIBLE_ARGS`](#settings) or Ansible's config.
- **`package.version`** ([refused](#what-is-refused)).
- **Creating infrastructure.**

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | `ansible-playbook` (ansible-core; `pip install ansible-core`) |
| The host | What Ansible needs: SSH and Python, or `--connection local` for this machine. Connection settings go in [`MERIDIAN_ANSIBLE_ARGS`](#settings) |
| To compile only | Nothing. `meridian compile` never runs Ansible |

Tested with ansible-core 2.19.

## A first document

[`examples/ansible/web-server.yaml`](../../examples/ansible/web-server.yaml), from
[your first ResourceSet](../quickstart/first-resourceset.md):

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

It compiles to three tasks and a handler:

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

It also compiles for Chef, Salt and cloud-init, and for Puppet and OpenVox with a restart
instead of a reload. See the [conformance matrix](../reference/conformance-matrix.md).

## Resource types

Unlisted params (`unsupported-param`) and states (`unsupported-state`) are refused.

| Type | Module | States (`state`) | Params | Required |
|---|---|---|---|---|
| `package` | `ansible.builtin.package` | `present` (default), `absent`, `latest` | `name` | `name` |
| `service` | `ansible.builtin.service` | `running` (`started`, default), `started`, `stopped`, `restarted`, `reloaded` | `name`, `enabled` | `name` |
| `file` | `ansible.builtin.copy` when there is `content`, `ansible.builtin.file` otherwise | `present` (`file`, default), `file`, `directory`, `absent` | `path` (`dest` or `path`), `content`, `owner`, `group`, `mode` | `path` |
| `user` | `ansible.builtin.user` | `present` (default), `absent` | `name`, `uid`, `home`, `shell`, `system`, `groups` | `name` |
| `group` | `ansible.builtin.group` | `present` (default), `absent` | `name`, `gid`, `system` | `name` |
| `exec` | `ansible.builtin.command`, or `ansible.builtin.shell` with `shell: true` | none; an exec has no state | `command` (`cmd`), `chdir`, `creates`, `removes`, `shell` | `command` |

- The **id is the task name** (one line, not empty); changes are read back by it.
- `exec` with `shell: true` uses `ansible.builtin.shell`; otherwise `ansible.builtin.command`.
- `exec` runs every play unless `creates` or `removes` is set; without them check mode skips
  it and the plan is unknown.
- Quote `mode` (`"0644"`).

## Modules beside the six

Any module is a type by its full name (`ansible.builtin.template`, `community.general.ufw`).
`params` are its arguments, passed through; Ansible checks them when the play runs, or the
editor once [synced](#editor-schemas).

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
uses both. Such a document compiles for Ansible only; the
[`native-type`](../reference/lint.md#native-type) lint marks it. `native/file` is refused
here; see
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Ordering and notifications

`dependsOn` becomes task order, with a warning naming independent resources.

`notifies` becomes a handler such as `reload nginx_svc`, run once at the end of the play if
a notifier changed:

| Notified type | Actions | What the handler does |
|---|---|---|
| `service` | `restart`, `reload`, `start`, `stop` | `ansible.builtin.service` with `state: restarted`, `reloaded`, `started` or `stopped` |
| `exec` | `run` | runs the command again |
| a module named in full | `run` | runs the module's task again, as written |
| anything else | none | refused: `unsupported-action` |

`runtimeWhen` is copied into `when:` as an Ansible expression:

```yaml
runtimeWhen: "ansible_facts['os_family'] == 'Debian'"
```

Handlers carry no condition.

## What a compile writes

```
playbook.yml
inventory.ini
```

`playbook.yml` holds one task per resource and one handler per distinct notification.
`inventory.ini` holds the host in group `meridian`.

- Jinja you write, such as `{{ ansible_facts['os_family'] }}`, runs.
- Values from `--data`, `--set`, data layers or `env()` are escaped (`{{ '{{' }}`), so they
  never run on the control machine
  ([see](../reference/dsl/interpolation.md#a-tools-own-expressions)).

A host that is not a name is refused (`invalid-host`;
[`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)).

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) compiles to
`{{ meridian_secret_<name> }}` in a task marked `no_log: true`. The value goes in a private
extra-vars file, passed as `--extra-vars @<path>` and removed after the run.

## Importing

`meridian import site.yml` writes one ResourceSet per play
([guide](../operations/importing.md)). `site.yml`, `playbook.yml` and names containing
`playbook` need no `--from ansible`.

- A task one of the six fully carries becomes that type.
- Any other becomes the module, with every argument; the document is then Ansible-only.
- Nothing becomes an `exec`.

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

Files are read as YAML 1.1 (`yes` is `true`). Reported:

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

Round-trip tests: [`web/site.yml`](../../plugins/ansible/testdata/import/web/site.yml) to
[`web.meridian.yaml`](../../plugins/ansible/testdata/import/web.meridian.yaml), compared with
[`web.expected.playbook.yml`](../../plugins/ansible/testdata/import/web.expected.playbook.yml);
and [`odd.meridian.yaml`](../../plugins/ansible/testdata/import/odd.meridian.yaml) with
[`odd.notes`](../../plugins/ansible/testdata/import/odd.notes).

## Running

| | |
|---|---|
| Plan | `ansible-playbook --inventory inventory.ini --check --diff playbook.yml` |
| Apply | `ansible-playbook --inventory inventory.ini playbook.yml` |

Both force the `default` callback with colour off. A `changed:` line under
`TASK [nginx_conf]` is a change to `nginx_conf`; a loop reports once.

A task check mode skips makes the plan **unknown**. A failed task fails the stage with what
had changed; exit `2` is a failed task, `4` an unreachable host.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_ANSIBLE_PLAYBOOK` | the program to run, when `ansible-playbook` is not on `PATH` |
| `MERIDIAN_ANSIBLE_ARGS` | arguments added to every run, split like a shell command line (quotes hold a word together): `--user deploy --private-key ~/.ssh/deploy`, or `--connection local` for this machine |
| `MERIDIAN_ANSIBLE_DOC` | the `ansible-doc` program `meridian schema sync` runs, when it is not on `PATH` |
| `MERIDIAN_ANSIBLE_GALAXY` | the `ansible-galaxy` program `meridian schema sync` runs for the collections' versions, when it is not on `PATH` |

Settings can also go in `meridian.yaml` under `spec.settings.ansible`; the environment
wins. See the [plugin settings reference](../reference/plugin-settings.md#ansible).

## Reaching the host as a credential

`targetHost.credential` names an `ssh` or `usernamePassword`
[credential](../reference/dsl/credentials.md).

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

Refused (`unsupported-credential`): a key `passphrase` or `certificate` (use your own agent,
`agent: true`), a `domain`. The plan names the identity: `on web01.internal as deploy (ssh,
user deploy)`. Do not also set `--user` or `--private-key` in `MERIDIAN_ANSIBLE_ARGS`.

## Editor schemas

`meridian schema sync` asks `ansible-doc` for the modules documents name
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | each module the documents name: every option with its type (`str`, `path`, `bool`, `int`, `float`, `list`, `dict`; `raw` and `json` as anything), whether it is required, its choices, its sub-options, and a `no_log` option as sensitive. Read from `ansible-doc -t module --json <module>`, one run a module, so a module that is not installed is a note and not the end of the sync |
| Listed by name | every other module `ansible-doc --list` finds, with its short description: ansible-core's own and every installed collection's |
| Pinned | ansible-core's version as `ansible.builtin`, and each installed collection's version from `ansible-galaxy collection list`. A collection installed under several paths is pinned at its newest version |

Aliases are not listed. `state` may be a param or the resource's `state`.

## What is refused

Each names the resource, line and rule:

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

- **Golden files** in `plugins/ansible/testdata/`, warnings and refusals included.
- **Schema tests** (`plugins/ansible/src/schemas.rs`) on saved `ansible-doc` output.
- **Conformance tests** generate the [conformance matrix](../reference/conformance-matrix.md).
- **Run tests** (`plugins/ansible/tests/run.rs`) against a stand-in `ansible-playbook`.
- **Import tests** (`plugins/ansible/tests/import.rs`).
- **Real-tool tests** (`plugins/ansible/tests/real.rs`, `ansible.yml`) run ansible-core 2.19
  locally: plan, apply, drift, handlers, `runtimeWhen`, failures, secrets, credentials,
  native modules and `ansible-doc`. Run with
  `MERIDIAN_TEST_REAL_ANSIBLE=1 cargo test -p meridian-target-ansible --test real -- --ignored`.
  SSH and the package, service, user and group modules have only met stand-ins. See
  [Real-tool tests](../contributing/real-tool-tests.md).
