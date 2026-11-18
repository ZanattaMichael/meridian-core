# Tool-wide settings live under `spec.settings`, not in a resource

| | |
|---|---|
| Area | Language |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "DSC takes its runner settings from spec.settings, not a resource" (issue 167) |
| Where it shows | [`settings`](../reference/dsl/index.md#settings), [DSC](../targets/dsc.md#how-the-configuration-is-run), [Writing a plugin](../contributing/writing-a-plugin.md), [`meridian plugins`](../reference/cli.md#plugins) |

## Context

The `dsc` target took how a configuration is run (the engine, how the host is
reached, the runner's credential name) as the params of one resource of type
`runner`. The owner refused that in issue 167: resources are for actual
resources, and how the document is run belongs to the document. The
[language expansion plan](../plans/language-expansion.md#53-settings) had
already proposed one `spec.settings` mapping for every target's tool-wide
settings, with the pseudo-resources kept, warned about, and removed in the
next `apiVersion`.

## The choice

`spec.settings` is a field of both document kinds: one mapping of the tool's
own settings for the whole document, which the target checks as it checks a
resource's `params`. Interpolation is placed in it as in a param, with the
target's `placed_text` escapes, before the plugin sees it.

A plugin declares the keys it reads with `Emitter::settings_keys`, which
`Describe` carries to the host and `meridian plugins` prints. The host refuses
a document that sets any other key before the plugin is asked, and a plugin that
declares none takes none. The SDK makes the same check in front of every `emit`,
so a plugin never has to. The alternative, letting a plugin ignore what it does
not read, was rejected: a setting nothing reads would be a silent loss, and a
plugin built before the field existed would drop every setting that way.

The `dsc` target is the first to move. It reads `engine`, `target` and
`credential` from `spec.settings`, under the runner's own names, so the move is
a plain cut and paste. A `runner` resource still compiles with a warning that
names the keys to move; one that contradicts `spec.settings` is refused
(`conflicting-settings`), and the type goes with the next `apiVersion`. The
importer writes `spec.settings`. The other targets' settings resources (`stack`,
`cluster`, `project`, `deployment`, Helm's four) are left as they are here, to
move the same way one target at a time.

The DSC `credential` setting stays the runner's own credential name, separate
from `targetHost.credential`: the `dsc` target does not take Meridian credentials
yet, and the two are different things until it does.

## Alternatives

- Nesting the runner's settings under `targetHost`, as the issue sketches.
  Rejected: `engine` is not about the host, an Infrastructure document has no
  `targetHost`, and the plan's shape is one mapping that every target can use.
- Refusing the `runner` resource outright. Rejected: the plan says a
  pseudo-resource keeps working with a warning until the next `apiVersion`,
  and the warning tells an author exactly what to move.
- Checking the keys only inside the plugin, with no `Describe` field.
  Rejected: a plugin built before `spec.settings` would silently drop it.

## Status

Proposed. Open to the owner: whether the `runner` resource should be refused
now rather than warned about, and the order the other targets' settings
resources move in.
