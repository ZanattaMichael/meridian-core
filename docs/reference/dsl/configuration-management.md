# The configuration management flavour

Configuration management tools log in to a machine, or run on it, and converge
packages, files and services. This page is how the
[Meridian language](index.md) is written for them.

| | |
|---|---|
| Category, as `meridian plugins` prints it | `configuration-management` |
| Kind of document | `ResourceSet` |
| The machine | `spec.targetHost`, required |
| Outputs | none declared: a ResourceSet has no `outputs`. `cloud-init` reports two of its own (where it wrote the seed) |

## Targets

| Target | Types | Page |
|---|---|---|
| `ansible` | the six shared types | [ansible](../../targets/ansible.md) |
| `puppet` | the six shared types | [puppet](../../targets/puppet.md) |
| `chef` | the six shared types | [chef](../../targets/chef.md) |
| `salt` | the six shared types | [salt](../../targets/salt.md) |
| `openvox` | the six shared types, compiled the way `puppet` compiles them | [openvox](../../targets/openvox.md) |
| `cloud-init` | the six shared types, limited to what a first boot can do | [cloud-init](../../targets/cloud-init.md) |
| `dsc` | DSC's own resources | [dsc](../../targets/dsc.md) |

There are two dialects in this segment, and the table shows which target speaks
which.

## The shared dialect: six types

`ansible`, `puppet`, `openvox`, `chef`, `salt` and `cloud-init` take the same six
resource types. A
resource written with them compiles for any of them, so changing `spec.target` is
all it takes to move a document from one tool to another, within what each tool
can do. This is the one place where the language is "write once".

| `type` | Manages | `state` | Required params |
|---|---|---|---|
| `package` | an installed package | `present` (default), `absent`, `latest` | `name` |
| `service` | a running service | `running` (default), `started`, `stopped` | `name` |
| `file` | a file or directory | `present` (default) or `file`, `directory`, `absent` | `path` |
| `user` | a user account | `present` (default), `absent` | `name` |
| `group` | a group | `present` (default), `absent` | `name` |
| `exec` | a command to run | none | `command` |

Every param, every state and which actions each type can be notified with are
in [the resource types reference](../resource-types.md).

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
and it compiles for all three tools. A longer one for each tool is under
[`examples/ansible/`](../../../examples/ansible),
[`examples/puppet/`](../../../examples/puppet) and
[`examples/chef/`](../../../examples/chef).

Anything outside the six types and their params is refused when the document is
compiled: an unknown type, a param the type does not have, a state the tool
cannot express. Nothing is dropped.

In this dialect a reference is settled when the document is compiled:
`${id.param}` becomes the value the document gives that param on the resource
`id`. Anything else in `${...}` is refused, and `$${` is a literal `${`. See
[references](references.md#configuration-management-the-shared-types).

### cloud-init: the same types, once, at first boot

`cloud-init` takes the six types, but it is not pointed at a running machine. It
compiles a ResourceSet into the user-data a new machine applies by itself the
first time it boots. So it accepts only what a first boot can do: nothing can be
removed or tracked later (`absent` and `latest` are refused), the order is the
fixed order of cloud-init's own boot stages (a dependency that points backwards
through them is refused), and it adds a few params of its own, such as a user's
`ssh_authorized_keys`. Use it for what must be true before any other tool can
reach the machine. [Its page](../../targets/cloud-init.md) has the detail, and
[the resource types reference](../resource-types.md) marks every difference.

## The native dialect: DSC

`dsc` does not take the six types. DSC's vocabulary is its resources, and there
are too many, with too many properties each, to hide behind six words.

| | |
|---|---|
| `type` | the DSC resource, `Module/Resource`: `PSDscResources/Service`, `Microsoft.Windows/Registry` |
| `params` | the resource's properties, exactly as the resource documents them |
| `state` | not used, and refused. A DSC resource carries its desired state in its own properties (`Ensure`, `_exist`) |
| Settings | [`spec.settings`](index.md#settings): the engine (DSC v2 or v3), how the host is reached, and the name of a credential the runner resolves. A resource of type `runner`, the old way to say the same, still compiles with a warning |
| References | `${id.Property}` reads a property another resource reported; the reader is ordered after it |

See [the `dsc` target](../../targets/dsc.md) and
[`examples/dsc/web-server.yaml`](../../../examples/dsc/web-server.yaml).

A document written in one dialect does not compile for a target of the other.

## Order

`dependsOn` works on every target in the segment. What it turns into depends on
how the tool thinks about order:

| Target | `dependsOn` becomes |
|---|---|
| `ansible`, `chef` | the order of the tasks or recipe. These tools run top to bottom, so resources with no dependency between them still run one after another, and the compile says which |
| `puppet`, `openvox` | `require` |
| `salt` | `require`; the states are also written in apply order |
| `cloud-init` | nothing written: each resource is placed at the step of the boot that handles it; a dependency that points backwards through those steps is refused |
| `dsc` | the runner's `dependsOn` |

## `notifies`

Every tool in the segment that runs against a live machine has its own
mechanism for reacting to a change, so a notify is **native** there: an Ansible
handler, Puppet's and OpenVox's `notify`, Chef's `notifies`, Salt's `watch` and
`onchanges`, the DSC runner's `notify`.
`cloud-init` has none and runs once, so its notify is **synthetic**: the action
becomes a command that runs once after every other command, unconditionally, and
the compile warns.

| Target | Actions accepted |
|---|---|
| `ansible` | `restart`, `reload`, `start`, `stop` on a service; `run` on an exec |
| `chef` | `restart`, `reload`, `start`, `stop` on a service; `run` on an exec |
| `salt` | `restart`, `reload`, `start`, `stop` on a service; `run` on an exec |
| `puppet`, `openvox` | `restart` (or `refresh`) on a service; `run` on an exec. `reload`, `start` and `stop` are refused: Puppet's refresh restarts a service and takes no argument |
| `dsc` | `refresh`: the notified resource's `Set` runs even when its `Test` says there is nothing to do |
| `cloud-init` | `start`, `stop`, `restart`, `reload` on a service; `run` on an exec |

A notified `exec` runs in its place in the order, and once more when it is
notified.

## `runtimeWhen`

A condition the tool checks on the machine at the moment it applies the
resource. Meridian does not read it. It is written in the tool's own language,
so it is the one part of a shared-dialect document that does not move between
tools.

| Target | `runtimeWhen` is | Example |
|---|---|---|
| `ansible` | a task's `when:` expression | `"ansible_facts['os_family'] == 'Debian'"` |
| `chef` | the Ruby of an `only_if` guard | `"node['platform_family'] == 'debian'"` |
| `salt` | a Jinja expression over grains and pillar, which the minion evaluates when it renders the file | `"grains['os_family'] == 'RedHat'"` |
| `dsc` | a `preCondition` in the runner's function language | `"equals (nodeName) 'web'"` |
| `puppet`, `openvox` | refused. A catalog is compiled before it reaches the node, so nothing is left to evaluate at apply time | |
| `cloud-init` | refused. cloud-init evaluates nothing per resource | |

## Plan and apply

`meridian plan` uses each tool's own dry run (`--check --diff`, `--noop`,
`--why-run`, the DSC runner's `Test` mode). Where a tool has none for the way
the host is reached, the plan is reported as unknown and not as empty. Each
target's page lists the exact commands under "Running".

## What this flavour does not have

- Outputs. Nothing a ResourceSet does is handed to a later stage.
- A way to create the machine. That is an Infrastructure document, joined to
  this one through `targetHost` with `from: infrastructure`
  ([two-stage workflow](../../quickstart/first-two-stage.md)).
- A `when` that depends on the node. [`when`](index.md#when-and-runtimewhen) is
  decided from the `--data` files before compiling, on every target; facts
  about the node are not gathered yet.
