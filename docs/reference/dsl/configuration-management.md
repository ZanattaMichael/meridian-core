# The configuration management flavour

How the [Meridian language](index.md) is written for tools that configure an existing
machine.

| | |
|---|---|
| Category, as `meridian plugins` prints it | `configuration-management` |
| Kind of document | `ResourceSet` |
| The machine | `spec.targetHost`, required |
| Outputs | none declared: a ResourceSet has no `outputs`. `cloud-init` reports two of its own (where it wrote the seed) |

## Targets

| Target | Types | Page |
|---|---|---|
| `ansible` | the six shared types, and any module by its full name (`ansible.builtin.lineinfile`) | [ansible](../../targets/ansible.md) |
| `puppet` | the six shared types, and any Puppet type (`cron`, `mysql::db`, `native/file`) | [puppet](../../targets/puppet.md) |
| `chef` | the six shared types, and any Chef resource (`template`, `native/package`) | [chef](../../targets/chef.md) |
| `salt` | the six shared types, and any state function (`pkgrepo.managed`) | [salt](../../targets/salt.md) |
| `openvox` | the six shared types and any Puppet type, compiled the way `puppet` compiles them | [openvox](../../targets/openvox.md) |
| `cloud-init` | the six shared types, limited to what a first boot can do, and any cloud-config module (`ntp`) | [cloud-init](../../targets/cloud-init.md) |
| `dsc` | DSC's own resources | [dsc](../../targets/dsc.md) |

Two dialects. Shared-dialect targets also take [native types](#native-types-beside-the-six).

## The shared dialect: six types

`ansible`, `puppet`, `openvox`, `chef`, `salt` and `cloud-init` take the same six types.
Changing `spec.target` moves a document between them, within each tool's limits.

| `type` | Manages | `state` | Required params |
|---|---|---|---|
| `package` | an installed package | `present` (default), `absent`, `latest` | `name` |
| `service` | a running service | `running` (default), `started`, `stopped` | `name` |
| `file` | a file or directory | `present` (default) or `file`, `directory`, `absent` | `path` |
| `user` | a user account | `present` (default), `absent` | `name` |
| `group` | a group | `present` (default), `absent` | `name` |
| `exec` | a command to run | none | `command` |

Params, states and actions: [resource types](../resource-types.md).

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
        path: /etc/nginx/nginx.conf
        content: "worker_processes 4;\n"
        mode: "0644"
      dependsOn:
        - resource: nginx_pkg
      notifies:
        - resource: nginx_svc
          action: restart

    - id: nginx_svc
      type: service
      state: running
      params:
        name: nginx
        enabled: true
      dependsOn:
        - resource: nginx_pkg
```

This document is
[`examples/flavours/configuration-management.yaml`](../../../examples/flavours/configuration-management.yaml),
and compiles for all three tools. Longer ones are under
[`examples/ansible/`](../../../examples/ansible),
[`examples/puppet/`](../../../examples/puppet) and
[`examples/chef/`](../../../examples/chef).

An unknown type, unsupported param or inexpressible state is refused at compile.

`${id.param}` becomes that param's value at compile time. Anything else in `${...}` is
refused; `$${` is a literal `${`
([references](references.md#configuration-management-the-shared-types)).

### cloud-init: the same types, once, at first boot

`cloud-init` compiles a ResourceSet into user-data applied on first boot:

- `absent` and `latest` are refused.
- Order follows cloud-init's boot stages; a dependency pointing backwards is refused.
- It adds params such as a user's `ssh_authorized_keys`.

See [its page](../../targets/cloud-init.md) and [resource types](../resource-types.md).

## Native types beside the six

A `type` that is not one of the six is the tool's own type, with `params` passed through
unchanged. The tool refuses unknown arguments when it runs; with a
[synced schema](../../tooling/schema-sync.md) the editor and `meridian lint` flag them.

| Target | The native `type` is | Example | `state` | When notified |
|---|---|---|---|---|
| `ansible` | the module's fully qualified collection name | `ansible.builtin.lineinfile`, `community.general.ufw` | the module's `state` argument | runs the task again (`run`) |
| `puppet`, `openvox` | the type as a declaration names it, `native/<name>` for the six | `cron`, `mysql::db`, `native/file` | the type's `ensure` | refreshed (`refresh`) |
| `chef` | the resource as a recipe names it, `native/<name>` for five (`execute` has no prefix) | `template`, `apt_update`, `native/package` | the resource's `action` | takes the action word as written |
| `salt` | the state function, dotted; `native/<name>` refused with the dotted name | `pkgrepo.managed`, `file.symlink` | refused: the function is the state | watches the notifier (`watch`) |
| `cloud-init` | the cloud-config key; `native/<name>` refused, as are the five keys the six write | `ntp`, `timezone`, `bootcmd` | refused: the module's keys say what it does | refused: applied once |

Rules:

- **A bare word is the shared type.** A dotted or namespaced name is the tool's.
- **A native type whose bare name a shared type has taken is written
  `native/<name>`**: `native/file` is the tool's own `file`, with every
  attribute the shared type does not take. The prefix is accepted only for the
  six names, and only where the tool has a bare type of that name; a target
  whose types are always fully qualified refuses it and says what to write
  (`ansible.builtin.file` on Ansible).
- **`dependsOn`, `notifies` and `runtimeWhen` work where the tool can honour them.** The
  target page says what is refused.
- **A native type makes the document single-target.** The
  [`native-type`](../lint.md#native-type) lint rule marks each one.

```yaml
resources:
  - id: nginx_pkg
    type: package                        # shared: compiles for every target
    params:
      name: nginx
  - id: worker_count
    type: ansible.builtin.lineinfile     # Ansible's own: compiles for ansible only
    params:
      path: /etc/nginx/nginx.conf
      regexp: "^worker_processes"
      line: worker_processes 4;
    dependsOn:
      - resource: nginx_pkg
    notifies:
      - resource: nginx_svc
        action: reload
  - id: nginx_svc
    type: service
    params:
      name: nginx
```

See each target's page and
[decision 0032](../../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## The native dialect: DSC

`dsc` takes DSC's own resources, not the six types.

| | |
|---|---|
| `type` | the DSC resource, `Module/Resource`: `PSDscResources/Service`, `Microsoft.Windows/Registry` |
| `params` | the resource's properties, exactly as the resource documents them |
| `state` | not used, and refused. A DSC resource carries its desired state in its own properties (`Ensure`, `_exist`) |
| Settings resource | one optional resource of type `runner`: the engine (DSC v2 or v3), and how the host is reached |
| References | `${id.Property}` reads a property another resource reported; the reader is ordered after it |

See [the `dsc` target](../../targets/dsc.md) and
[`examples/dsc/web-server.yaml`](../../../examples/dsc/web-server.yaml).

A document written in one dialect does not compile for a target of the other.

## Order

`dependsOn` works on every target:

| Target | `dependsOn` becomes |
|---|---|
| `ansible`, `chef` | the order of the tasks or recipe. These tools run top to bottom, so resources with no dependency between them still run one after another, and the compile says which |
| `puppet`, `openvox` | `require` |
| `salt` | `require`; the states are also written in apply order |
| `cloud-init` | nothing written: each resource is placed at the step of the boot that handles it; a dependency that points backwards through those steps is refused |
| `dsc` | the runner's `dependsOn` |

## `notifies`

**Native** everywhere except `cloud-init`: an Ansible handler, Puppet and OpenVox
`notify`, Chef `notifies`, Salt `watch` and `onchanges`, the DSC runner's `notify`. On
`cloud-init` it is **synthetic**: a command run once, unconditionally, after every other,
with a warning.

| Target | Actions accepted |
|---|---|
| `ansible` | `restart`, `reload`, `start`, `stop` on a service; `run` on an exec or on a module named in full |
| `chef` | `restart`, `reload`, `start`, `stop` on a service; `run` on an exec; any action word on a Chef resource of its own |
| `salt` | `restart`, `reload`, `start`, `stop` on a service; `run` on an exec; `watch` on a state function of Salt's own |
| `puppet`, `openvox` | `restart` (or `refresh`) on a service; `run` on an exec; `refresh` on a Puppet type of its own. `reload`, `start` and `stop` are refused: Puppet's refresh restarts a service and takes no argument |
| `dsc` | `refresh`: the notified resource's `Set` runs even when its `Test` says there is nothing to do |
| `cloud-init` | `start`, `stop`, `restart`, `reload` on a service; `run` on an exec. A cloud-config module cannot be notified |

A notified `exec` runs in its place in the order, and once more when it is
notified.

## `runtimeWhen`

Checked by the tool on the machine at apply time, in the tool's own language. Meridian
does not read it, and it does not move between tools.

| Target | `runtimeWhen` is | Example |
|---|---|---|
| `ansible` | a task's `when:` expression | `"ansible_facts['os_family'] == 'Debian'"` |
| `chef` | the Ruby of an `only_if` guard | `"node['platform_family'] == 'debian'"` |
| `salt` | a Jinja expression over grains and pillar, which the minion evaluates when it renders the file | `"grains['os_family'] == 'RedHat'"` |
| `dsc` | a `preCondition` in the runner's function language | `"equals (nodeName) 'web'"` |
| `puppet`, `openvox` | refused. A catalog is compiled before it reaches the node, so nothing is left to evaluate at apply time | |
| `cloud-init` | refused. cloud-init evaluates nothing per resource | |

## `adopt`

Accepted with a warning on every target but `cloud-init`, and changes nothing:
these tools keep no state, and make each thing what the document says whether
it existed or not. `cloud-init` refuses it. See
[adopting what exists](resources.md#adopting-what-exists).

## Plan and apply

`meridian plan` uses the tool's dry run (`--check --diff`, `--noop`, `--why-run`, the DSC
runner's `Test` mode). Without one, the plan is unknown. Each target page lists the
commands.

## What this flavour does not have

- Outputs.
- Creating the machine; use an Infrastructure document and `targetHost`
  ([two-stage workflow](../../quickstart/first-two-stage.md)).
- A [`when`](index.md#when-and-runtimewhen) on node facts.
