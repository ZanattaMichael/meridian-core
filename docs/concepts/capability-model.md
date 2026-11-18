# The capability model

The tools Meridian drives are not one tool with different syntax. They differ
on ordering, reacting to change, checking conditions at apply time, and
previewing changes. A language over all of them can either approximate what you
asked, or be exact about what each tool can do.

Meridian is exact:

> A document either compiles into something that does what it says, or it does
> not compile. There is no third outcome.

## What a plugin declares

Every plugin states a few facts about its tool:

- whether it can react to a change natively, synthetically, or not at all
  (`notifies`)
- whether it can check a condition on the node at the moment of apply
  (`runtimeWhen`)
- whether it can plan, apply, and report outputs
- its category: infrastructure as code, configuration management,
  containerization or container orchestration
- the tool versions it compiles for, if that matters
- how its tool's own expression syntax opens, if not only `${`, so a value
  from data is written as text and never as an expression ([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions))

`meridian plugins` prints them, and the
[capability matrix](../reference/capability-matrix.md) is generated from them.

Meridian checks some of these itself, before the plugin sees a document:

- **Category** decides which kind of document the plugin gets. A ResourceSet
  aimed at a provisioning tool, or an Infrastructure document aimed at a
  machine-configuration tool, is refused
  ([the pairing](../reference/dsl/index.md#the-document)).
- **Versions** decide which [`targetVersion`](../reference/dsl/index.md#targetversion)
  a document may ask for. A plugin that declares none is never asked for one.
- **Settings keys** decide which [`spec.settings`](../reference/dsl/index.md#settings)
  a document may set. A plugin that declares none takes none.

Plugin authors declare these by implementing `category()`, `target_versions()`
and `settings_keys()` on the SDK's `Emitter`.

The same rule covers Meridian's own fields. `metadata.labels`, `when`,
`ifMissing` and a notify's `on` do what the
[language reference](../reference/dsl/index.md) says on every target, or the
document is refused. No field is read and then ignored.

## What happens when a document asks for more

It is refused at compile time, before anything runs, with the file, line,
resource and reason:

```
site.yaml:37:11: puppet: resource "nginx_conf" [unsupported-action]: notifies
resource "nginx_svc" with action "reload", which this target cannot express:
Puppet's refresh restarts a service; it takes no argument and cannot be told to
reload instead. ...
```

Each refusal has a rule name in brackets and a hint in parentheses after the
message. Rules come from the [refusal rules](../reference/refusal-rules.md) and
are the same for the same mistake on every target. The common ones:

| Rule | Meaning |
|---|---|
| `unsupported-resource-type` | the target has no such type; the message lists the ones it has |
| `unsupported-param` | the type does not take that param on this target; it is never dropped silently |
| `unsupported-state` | the type cannot be in that state on this target |
| `unsupported-action` | a notify asks for something the tool cannot do to that resource |
| `unsupported-runtime-condition` | `runtimeWhen` on a target that cannot check at apply time |
| `invalid-reference` | `${...}` names something that is not there, or a value the tool cannot know at that point |
| `missing-param` | something the tool requires was not given |

## Warnings are not refusals in disguise

A warning means the output does what the document says, but something about
how is worth knowing. There are three kinds:

- The tool is less expressive than the document, without changing the result. Ansible runs independent resources in sequence; `kubectl` does not
  wait between manifests.
- A notify is synthetic, and appears in the tool's output as a checksum label,
  an annotation, a replacement, or a command at the end of first boot.
- The output relies on something about the machine the document cannot check.
  cloud-init has no services module, so a service becomes `systemctl` commands
  and the image must use systemd.

## Why plan can be "unknown"

A plan lists what an apply would change. Sometimes a tool cannot tell: Azure
cannot analyse some resource types, and `chef-run` has no dry run. Meridian then
reports the plan as unknown, not "no changes", because a possibly wrong empty
list is worse than none.
