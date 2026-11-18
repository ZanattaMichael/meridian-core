# YAML pitfalls, and what Meridian does about each

Only `true` and `false` are booleans. Quote file modes and version numbers. Everything else
is read as it looks.

## Contents

- [The table](#the-table)
- [Booleans: the Norway problem](#booleans-the-norway-problem)
- [Numbers that lose their spelling](#numbers-that-lose-their-spelling)
- [Octal, and a leading zero](#octal-and-a-leading-zero)
- [Dates and times](#dates-and-times)
- [Tags](#tags)
- [Indentation, tabs and duplicate keys](#indentation-tabs-and-duplicate-keys)
- [Templating](#templating)
- [Flow style, KYAML and JSON](#flow-style-kyaml-and-json)
- [What Meridian writes](#what-meridian-writes)

## The table

| You write | Meridian reads | What to do |
|---|---|---|
| `yes`, `no`, `on`, `off`, `y`, `n` | text | nothing; only `true` and `false` are booleans |
| `true`, `True`, `FALSE` | a boolean | nothing |
| `0644` | the integer 420 | quote a file mode: `"0644"`. The [`unquoted-mode`](../lint.md#unquoted-mode) lint says so, and on the configuration management types it is a compile error |
| `08`, `09` | text | nothing; these are not octal and no number was meant |
| `1.10`, `3.0`, `2.50` | the numbers 1.1, 3 and 2.5 | quote a version or a tag: `"1.10"`. The [`lossy-number`](../lint.md#lossy-number) lint says so |
| `1_000`, `+7`, `1e3`, `.5` | the numbers 1000, 7, 1000 and 0.5 | quote it if the spelling matters; `lossy-number` says so |
| `0x1F`, `0o17`, `0b101` | the integers 31, 15 and 5 | nothing |
| `11:00` | text | nothing; nothing is read in base 60 |
| `2026-11-18`, `2026-11-18T09:00:00Z` | text | nothing; a date is never turned into a timestamp |
| `~`, `null`, nothing at all | null | nothing |
| `!!str 42` | the text `42` | nothing; quoting does the same |
| `!!int "12"`, `!!float 1`, `!!binary ...`, `!custom x` | an error | write the value plainly, or quote it for text |
| a tab in the indentation | an error | use spaces |
| the same key twice in one mapping | an error | remove one |
| `&anchor`, `*alias`, `<<: *merge` | supported | nothing |
| `{a: 1}`, `[1, 2]`, KYAML, JSON | the same document as the block form | nothing; `meridian fmt` leaves a flow-style document as written |

## Booleans: the Norway problem

YAML 1.2 rules: only `true` and `false`, in any case, are booleans. `enabled: yes` is the
text `yes`, refused where a boolean is required. The same applies to
[data files](index.md#giving-the-data). Only [`meridian import`](../cli.md#import) reads
YAML 1.1, where `yes` is `true`.

## Numbers that lose their spelling

`1.10` is 1.1, `3.0` is 3, `1_000` is 1000, `+7` is 7. The
[`lossy-number`](../lint.md#lossy-number) lint flags a plain number whose canonical form
differs from what was typed; `meridian lint --fix` quotes it. Params declared as numbers
are not reported.

## Octal, and a leading zero

`0644` is octal 420. Quote modes: `mode: "0644"`. The `unquoted-mode` lint fixes it; on the
six configuration management types it is a compile error (`invalid-param`).

`08` and `09` are text.

## Dates and times

`2026-11-18` and `11:00` are text.

## Tags

Only `!!str` is allowed. Every other tag (`!!int`, `!!float`, `!!bool`, `!!binary`, `!!set`,
`!custom`) is refused with its line and column.

## Indentation, tabs and duplicate keys

Unknown fields are errors except inside `params` and `metadata.labels`; `dependsOn`
indented into `params` is reported as an unknown param. Tabs in indentation and duplicate
keys are errors. `meridian fmt` writes two spaces per level in field order.

## Templating

There is no text templating. An [interpolation](interpolation.md) is decided on the parsed
value: `count: ${{ data.replicas }}` is a number, a list or mapping keeps its shape, and
data cannot change the document's structure.

## Flow style, KYAML and JSON

Block style, flow style, [KYAML](https://kubernetes.io/docs/reference/encodings/kyaml/)
and JSON all parse to the same document; the form is detected from content.
`kubectl get -o kyaml` output imports the same as `-o yaml`.

- `meridian fmt` normalises block layout only; flow documents and JSON are left as written.
- `lossy-number` reports in block mappings only. `unquoted-mode` reports a mode in a flow
  mapping at the document's first line, without a fix.
- Meridian does not write KYAML.

## What Meridian writes

Every YAML-writing target uses one block-style writer. Strings a YAML 1.1 reader would
misread are quoted: `"no"`, `"yes"`, `"0644"`, `"08"`, `"1.10"`,
`"11:00"`, `"2026-11-18"`, `"12"`, `"null"`, and the keys `"no"` and `"on"`.
Multi-line strings are `|` blocks. Key order is fixed, so output is reproducible.
