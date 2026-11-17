# The capability model

The tools Meridian drives are not the same tool with different syntax. They
disagree about order, about reacting to change, about whether a condition can be
checked at the last moment, about whether they can tell you what they would do.
A single language over all of them can deal with that in two ways. It can
pretend, and quietly do something close to what you asked. Or it can be exact
about what each tool can do.

Meridian is exact. The rule is short:

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
- the versions of its tool it compiles for, if that matters for the tool
- the openings of its tool's own expression syntax, if it has any beyond
  `${`, so that a value placed from data is written as text and never as an
  expression ([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions))

`meridian plugins` prints them, and the
[capability matrix](../reference/capability-matrix.md) is generated from them.

The last two are checked by Meridian itself, before the plugin sees a document.
The category decides which kind of document the plugin is handed: a ResourceSet
aimed at a tool that provisions, or an Infrastructure document aimed at a tool
that configures one machine, is refused
([the pairing](../reference/dsl/index.md#the-document)). The versions decide
which [`targetVersion`](../reference/dsl/index.md#targetversion) a document may
ask for; a plugin that declares none is never asked for one. A plugin author
states them by implementing `category()` and `target_versions()` on the SDK's
`Emitter`.

The same rule covers the fields of the language that belong to Meridian and not
to a tool. `metadata.labels`, `when`, `ifMissing` and a notify's `on` each do
what the [language reference](../reference/dsl/index.md) says on every target,
or the document is refused. No field is read and then ignored.

## What happens when a document asks for more

It is refused when it is compiled, before anything runs, with the file, the line,
the resource and the reason:

```
site.yaml:37:11: puppet: resource "nginx_conf" [unsupported-action]: notifies
resource "nginx_svc" with action "reload", which this target cannot express:
Puppet's refresh restarts a service; it takes no argument and cannot be told to
reload instead. ...
```

Each refusal has a rule name in brackets. The common ones:

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

A warning means the output does what the document says, and there is something
about how that you should know. Three kinds exist today:

- The tool is less expressive than the document, in a way that cannot change the
  result. Ansible runs independent resources in sequence; `kubectl` does not
  wait between manifests.
- A notify is synthetic, and will show up in the tool's own output as a checksum
  label, an annotation, a replacement, or a command at the end of a first boot.
- The output relies on something about the machine that the document cannot
  check. cloud-init has no module for services, so a service becomes `systemctl`
  commands and the image has to use systemd.

## Why plan can be "unknown"

A plan lists what an apply would change. Sometimes a tool cannot say: Azure
cannot analyse a resource type, `chef-run` has no dry run. Meridian then reports
the plan as unknown. It does not report "no changes", because an empty list that
might be wrong is worse than no list.
