# Conformance matrix

What happens to the same document on each target that shares the
[configuration management resource types](resource-types.md). This page is
generated: `crates/meridian-cli/tests/conformance.rs` compiles one small
document per row with every target and writes down how each compile came out,
and it fails when this page is out of date. So the page cannot drift from the
code, and a plugin cannot start accepting or refusing something without the
change showing here.

How to read a cell:

- **ok**: the document compiled.
- **ok, warns**: it compiled, with a warning the plan shows. Nothing is dropped
  silently; the warning says what the tool will do differently.
- a name such as `unsupported-state`: the target refused the document, and that
  is the rule the refusal carries. The message that goes with it names the
  resource, the line, and what the target does accept.

The **same** column says whether the targets are allowed to differ. A row
marked *yes* is a part of the dialect that must come out the same everywhere,
and the test fails if one target breaks ranks. A row marked *no* is a place
where the tools themselves differ; the cells say how, and the
[resource types](resource-types.md) page and each target's own page say why.

## `package`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| no state (present) | yes | ok | ok | ok | ok | ok | ok |
| state `present` | yes | ok | ok | ok | ok | ok | ok |
| state `absent` | no | ok | ok | `unsupported-state` | ok | ok | ok |
| state `latest` | no | ok | ok | `unsupported-state` | ok | ok | ok |
| a state no tool has (`purged`) | yes | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` |
| param `version` | no | `unsupported-param` | `unsupported-param` | ok | `unsupported-param` | `unsupported-param` | ok |
| param `version` with state `latest` | no | `unsupported-param` | `unsupported-param` | `unsupported-state` | `unsupported-param` | `unsupported-param` | `param-conflicts-with-state` |
| no `name` | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| no `params` at all | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| a param outside the dialect (`provider`) | yes | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` |
| `name` is a number | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| `name` is a list | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |

## `service`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| no state (running) | yes | ok | ok | ok, warns | ok | ok | ok |
| state `running`, `enabled: true` | yes | ok | ok | ok, warns | ok | ok | ok |
| state `started` | yes | ok | ok | ok, warns | ok | ok | ok |
| state `stopped`, `enabled: false` | yes | ok | ok | ok, warns | ok | ok | ok |
| state `restarted` | no | ok | `unsupported-state` | ok, warns | `unsupported-state` | `unsupported-state` | `unsupported-state` |
| state `reloaded` | no | ok | `unsupported-state` | ok, warns | `unsupported-state` | `unsupported-state` | `unsupported-state` |
| a state no tool has (`paused`) | yes | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` |
| no `name` | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| `enabled` is text, not true or false | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| a param outside the dialect (`pattern`) | yes | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` |

## `file`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| content, owner, group and mode | yes | ok | ok | ok | ok | ok | ok |
| state `file` | yes | ok | ok | ok | ok | ok | ok |
| state `directory` | yes | ok | ok | ok | ok | ok | ok |
| state `directory` with `content` | no | `unsupported-state` | `invalid-param` | `unsupported-param` | `param-conflicts-with-state` | `param-conflicts-with-state` | `param-conflicts-with-state` |
| state `absent` | yes | ok | ok | ok | ok | ok | ok |
| state `absent` with owner and mode | no | ok | ok | `unsupported-param` | ok | ok | ok, warns |
| no `content` | no | ok | ok | `missing-param` | ok | ok | ok |
| empty `content` | no | ok | ok | `empty-param` | ok | ok | ok |
| content with quotes, a backslash and every tool's own interpolation | yes | ok | ok | ok | ok | ok | ok |
| content that is not ASCII | yes | ok | ok | ok | ok | ok | ok |
| content of several lines | yes | ok | ok | ok | ok | ok | ok |
| `mode` unquoted, so read as a number | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| no `path` | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| a state no tool has (`touched`) | yes | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` |
| a param outside the dialect (`source`) | yes | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` |

## `user`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| only `name` | yes | ok | ok | ok | ok | ok | ok |
| uid, home, shell, system and groups | yes | ok | ok | ok | ok | ok | ok |
| state `absent` | no | ok | ok | `unsupported-state` | ok | ok | ok |
| no `name` | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| `groups` is text, not a list | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| a param outside the dialect (`password`) | yes | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` |

## `group`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| only `name` | yes | ok | ok | ok | ok | ok | ok |
| `gid` and `system` | no | ok | ok | `unsupported-param` | ok | ok | ok |
| state `absent` | no | ok | ok | `unsupported-state` | ok | ok | ok |
| no `name` | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |

## `exec`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| only `command` | yes | ok | ok | ok | ok | ok | ok |
| `chdir` and `creates` | yes | ok | ok | ok | ok | ok | ok |
| `shell: true` and a pipe | yes | ok | ok | ok | ok | ok | ok |
| a pipe without `shell: true` | no | ok | ok | ok | ok | ok | `shell-required` |
| `removes` | no | ok | `unsupported-param` | ok | `unsupported-param` | `unsupported-param` | ok |
| a state (an exec has none) | yes | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` |
| no `command` | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| a param outside the dialect (`timeout`) | yes | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` |

## `types`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| a type that is no shared type and no tool's own (`Firewall.Rule`) | yes | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` |
| a type in the wrong case (`Package`) | yes | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` |
| all six types in one document | yes | ok, warns | ok | ok, warns | ok | ok | ok |

## `native types`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| an Ansible module in full (`ansible.builtin.lineinfile`) | no | ok | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` |
| an Ansible module in full, with a state | no | ok | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` |
| an Ansible module in full, notified with `run` | no | ok, warns | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` |
| a bare word that is no shared type (`firewall_rule`) | no | `unsupported-resource-type` | ok | ok | ok | ok | `unsupported-resource-type` |
| a bare module name (`lineinfile`) | no | `unsupported-resource-type` | ok | ok | ok | ok | `unsupported-resource-type` |
| the tool's own `file` under the prefix (`native/file`) | no | `unsupported-resource-type` | ok | `unsupported-resource-type` | ok | ok | `unsupported-resource-type` |
| a Puppet module type (`mysql::db`) | no | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | ok | ok | `unsupported-resource-type` |
| a Salt state function (`pkgrepo.managed`) | no | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | ok |
| a Chef resource (`template`), notified with `create` | no | `unsupported-resource-type` | ok | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-resource-type` |
| a cloud-config module (`ntp`) | no | `unsupported-resource-type` | ok | ok | ok | ok | `unsupported-resource-type` |
| the tool's own `package` under the prefix, with a state (`native/package`) | no | `unsupported-resource-type` | ok | `unsupported-resource-type` | ok | ok | `unsupported-resource-type` |
| a Puppet type notified with `refresh` | no | `unsupported-resource-type` | ok | `unsupported-action` | ok | ok | `unsupported-resource-type` |
| a Salt state function notified with `watch` | no | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | `unsupported-resource-type` | ok |
| a native type that depends on a shared one | no | `unsupported-resource-type` | ok | `unsupported-resource-type` | ok | ok | `unsupported-resource-type` |

## `dependsOn`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| a chain written in reverse order | yes | ok | ok | ok, warns | ok | ok | ok |
| a diamond | yes | ok, warns | ok | ok, warns | ok | ok | ok |
| the same dependency named twice | yes | ok | ok | ok, warns | ok | ok | ok |

## `notifies`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| a file restarts a service | yes | ok, warns | ok | ok, warns | ok | ok | ok |
| a file reloads a service | no | ok, warns | ok | ok, warns | `unsupported-action` | `unsupported-action` | ok |
| a file starts a service | no | ok, warns | ok | ok, warns | `unsupported-action` | `unsupported-action` | ok |
| a file stops a service | no | ok, warns | ok | ok, warns | `unsupported-action` | `unsupported-action` | ok |
| a file restarts a service whose state is `stopped` | no | ok, warns | ok | ok, warns | ok | ok | `unsupported-action` |
| a file runs an exec | yes | ok, warns | ok | ok, warns | ok | ok | ok |
| an action no tool has (`bounce`) | yes | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` |
| `restart` aimed at an exec | yes | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` |
| a package is notified (it cannot be) | yes | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` | `unsupported-action` |
| two files restart the same service | yes | ok, warns | ok | ok, warns | ok | ok | ok |
| one file notifies a service and an exec | yes | ok, warns | ok | ok, warns | ok | ok | ok |
| a chain: a package runs an exec, which restarts a service | yes | ok, warns | ok | ok, warns | ok | ok | ok |
| a resource notifies one it also depends on | no | ok | ok | `unsatisfiable-order` | ok | ok | ok |

## `references`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| a whole value: `${dir.path}` | yes | ok, warns | ok | ok | ok | ok | ok |
| inside text: `${dir.path}/index.html` | yes | ok, warns | ok | ok | ok | ok | ok |
| two references in one value | yes | ok, warns | ok | ok | ok | ok | ok |
| a reference to a reference | yes | ok, warns | ok | ok | ok | ok | ok |
| a whole value that is a number: `${usr.uid}` | yes | ok, warns | ok | ok | ok | ok | ok |
| a resource that is not in the document | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| a param the resource does not set | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| a list read into the middle of text | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| a resource that reads itself | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| two resources that read each other | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| something that is not `id.param`: `${upper(dir.path)}` | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| never closed: `${dir.path` | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| a literal: `$${HOME}` | yes | ok | ok | ok | ok | ok | ok |
| a dollar that starts nothing: `$HOME` and `$(id -u)` | yes | ok | ok | ok | ok | ok | ok |

## `runtimeWhen`

| Document | same | ansible | chef | cloud-init | openvox | puppet | salt |
|---|---|---|---|---|---|---|---|
| on an exec | no | ok | ok | `unsupported-runtime-condition` | `unsupported-runtime-condition` | `unsupported-runtime-condition` | ok |
| on a notified service | no | ok, warns | ok | `unsupported-runtime-condition` | `unsupported-runtime-condition` | `unsupported-runtime-condition` | ok |

