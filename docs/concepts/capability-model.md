# The capability model

A document either compiles into something that does what it says, or it does not
compile. Meridian never approximates.

## What a plugin declares

Each plugin declares:

- whether it can react to a change natively, synthetically or not at all (`notifies`)
- whether it can check a condition on the node at apply time (`runtimeWhen`)
- whether it can take an existing thing under management (`adopt`): by import, by
  converging, or not at all
- whether it can plan, apply and report outputs
- its category: infrastructure as code, configuration management, containerization or
  container orchestration
- the tool versions it compiles for, if any
- any expression syntax its tool reads besides `${`, so data is written as text
  ([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions))

`meridian plugins` prints them. The [capability matrix](../reference/capability-matrix.md)
is generated from them.

Meridian checks these before the plugin sees a document:

- **Category** decides which document kind the plugin takes
  ([the pairing](../reference/dsl/index.md#the-document)).
- **Versions** decide which [`targetVersion`](../reference/dsl/index.md#targetversion) a
  document may ask for. A plugin that declares none takes none.
- **Settings keys** decide which [`spec.settings`](../reference/dsl/index.md#settings) a
  document may set. A plugin that declares none takes none.

Plugin authors implement `category()`, `target_versions()` and `settings_keys()` on the
SDK's `Emitter`.

Meridian's own fields follow the same rule. `metadata.labels`, `when`, `ifMissing` and a
notify's `on` do what the [language reference](../reference/dsl/index.md) says on every
target, or the document is refused.

## Refusals

A document that asks for more than the target can do is refused at compile time, with
file, line, resource and reason:

```
site.yaml:37:11: puppet: resource "nginx_conf" [unsupported-action]: notifies
resource "nginx_svc" with action "reload", which this target cannot express:
Puppet's refresh restarts a service; it takes no argument and cannot be told to
reload instead. ...
```

The rule name is in brackets and a hint follows in parentheses. Rules are listed in
[refusal rules](../reference/refusal-rules.md) and mean the same on every target:

| Rule | Meaning |
|---|---|
| `unsupported-resource-type` | the target has no such type; the message lists the ones it has |
| `unsupported-param` | the type does not take that param on this target |
| `unsupported-state` | the type cannot be in that state on this target |
| `unsupported-action` | a notify asks for something the tool cannot do to that resource |
| `unsupported-runtime-condition` | `runtimeWhen` on a target that cannot check at apply time |
| `unsupported-adopt` | `adopt` on a target that cannot take an existing thing under management |
| `invalid-reference` | `${...}` names something that is not there, or a value the tool cannot know at that point |
| `missing-param` | something the tool requires was not given |

## Warnings

A warning means the output does what the document says, but how is worth knowing:

- The tool is less expressive without changing the result. Ansible runs independent
  resources in sequence; `kubectl` does not wait between manifests.
- A notify is synthetic: a checksum label, an annotation, a replacement, or a command at
  the end of first boot.
- The output relies on something the document cannot check. cloud-init has no services
  module, so a service becomes `systemctl` commands and the image must use systemd.
- A field changes nothing here: `adopt` on a stateless tool, which converges anyway.

## Unknown plans

When a tool cannot say what an apply would change, the plan is reported as unknown, not as
"no changes". Azure cannot analyse some resource types, and `chef-run` has no dry run.
