# Project settings are keyed by the plugin's short names, and the environment wins

| | |
|---|---|
| Area | Plugins |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the plugin settings work for issue 93 |
| Where it shows | [Plugin settings](../reference/plugin-settings.md), [the manifest](../reference/modules.md#plugin-settings) |

## Context

A runner was configured only through about sixty environment variables, each
a constant in a plugin's `runner.rs`, listed nowhere together, and every
`_ARGS` variable split on whitespace. Issue 93 asked for settings to be
declared, read one way, listed, and writable in `meridian.yaml`.

## The choice

Each plugin declares its settings (`Runner::settings`), each with a short name
such as `args`, the environment variable, a default and a description, and
reads them through one SDK module. `meridian.yaml` carries them under
`spec.settings.<target>.<name>`, keyed by the **short name**, not the
variable. The host hands a target its map in one environment variable when it
starts the plugin, and the SDK reads the variable first, the project second,
the default last: **the environment always wins**. A name the plugin does not
declare refuses to start it.

Argument lists are split like a POSIX shell command line, so quotes hold an
argument with a space together; an unterminated quote takes the rest.

## Alternatives

Keying `spec.settings` by the variable name (`MERIDIAN_ANSIBLE_ARGS`). Nothing
to look up, but shouting in a YAML file, and it ties the project file to how a
plugin happens to read its configuration rather than to what the setting is.

Letting the project win over the environment. A file committed with the
project would then override an operator's own shell, and a CI job could not
redirect one run without editing the file.

Refusing an unterminated quote instead of taking the rest. Runners are
constructed before any call that could report an error, so the choice was
between a lenient read and a panic; the lenient read matches what a shell
does with the same text.
