# A command's JSON result is one object, and a plan with changes exits 2 only when asked

| | |
|---|---|
| Area | Core |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | issue 101, machine-readable output for every command |
| Where it shows | [Machine-readable output](../reference/cli-json.md), [exit status](../reference/cli.md#exit-status) |

## Context

Meridian is meant to run in pipelines and to be wrapped by other tools, and
until now the only structured output was the event stream. Everything else
was text for a person, whose wording is changed freely to make it clearer, so
a wrapper parsing it broke whenever a message improved. A plan exited `0`
whether or not it would change anything, so a pipeline could not ask "is
there anything to apply?" without reading the text.

## The choice

`--output json` is one global flag, and in that mode every command prints
exactly one JSON object on standard output, at its end, with the report for
people moved to standard error, the way `--events json` already moves it.
Every object carries `command` and `ok`; the rest is the command's own, and
each shape has a JSON Schema under `schemas/output/` that a test checks every
command's output against. A command that fails before it has a result prints
an error object with the same two fields, so a reader never has to fall back
to standard error, and the error is still on standard error for the person.

The objects are built in the CLI from the same values the text printer
reads, not from new serialisation in the core crates: the orchestrator's
types stay plain, and the text and the object cannot disagree about what
happened. A sensitive output is the mask in the object exactly as in the
text.

`ok` means "the command did what was asked" and is false exactly when the
command exits `1` or `130`. A `plan` that found changes and a `drift` that
found drift are `ok`: the check did its job. What it found is a flag in the
object, and the exit status carries it too, as `2`.

`plan` keeps exiting `0` when it has changes, and `--detailed-exitcode` makes
it exit `2` instead, as Terraform's flag of the same name does and as `drift`
already does. A stage counts as having changes when its tool listed any, when
the tool cannot preview, or when the stage is deferred until an apply
produces an output it needs.

## Alternatives

Making a plan with changes exit `2` always. Every script that runs `plan`
today and checks for `0` would break, and `drift` only exits `2` because it
was born that way.

Printing JSON Lines, one object per stage. A pipeline wants the result as a
whole, with its summary, and the event stream already gives the stage by
stage view.

An `exitCode` field in every object. The exit status is the process's, and
putting it in the object as well invites the two to disagree; `ok` and the
summary flags say what a reader needs.

Serialising the core types with `serde` and deriving the schemas. The
orchestrator does not depend on `serde`, and a derived schema would expose
every internal field as the interface. The shapes are chosen by hand so that
they stay stable while the types change.
