# YAML pitfalls, and what Meridian does about each

YAML is the language Meridian documents are written in, and YAML has traps
that have bitten every tool that uses it: `no` read as a boolean, `0644` read
as a number, `1.10` read as `1.1`, a misplaced indent that is valid YAML
meaning something else. Kubernetes answered them in 2025 with
[KYAML](https://kubernetes.io/docs/reference/encodings/kyaml/), a strict
subset of YAML that `kubectl` can print. This page says, trap by trap, what
Meridian does, so an author knows what to quote and what is safe to leave
alone.

The short version: only `true` and `false` are booleans, a file mode and a
version number are quoted, and everything else is read the way it looks.

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

YAML 1.1 read `no`, `NO`, `n`, `off`, `yes`, `y` and `on` as booleans, so a
country code of `NO` became `false`. Meridian reads YAML by the YAML 1.2
rule: `true` and `false`, in any capitalisation, are booleans, and nothing
else is. `enabled: yes` is the text `yes`, and on a resource type whose
schema wants a boolean that is refused with a message saying to write `true`.

The same holds for [data files](index.md#giving-the-data) given with
`--data`. The one place Meridian reads YAML 1.1 on purpose is
[`meridian import`](../cli.md#import), because Ansible and Kubernetes
files were written for readers that do; there `enabled: yes` means `true`,
as it did to the tool.

## Numbers that lose their spelling

A plain scalar that looks like a number becomes one, and a number has one
spelling. `1.10` is the float 1.1, `3.0` is 3, `1_000` is 1000, `+7` is 7.
Where the value was a version, an image tag, a build number or an id, that
is the wrong value, and it reaches the tool that way.

Meridian cannot know which numbers were meant as text, so the
[`lossy-number`](../lint.md#lossy-number) lint points at every plain number
whose canonical spelling differs from what was typed, and `meridian lint --fix`
(or the editor's quick fix) quotes it. A number whose spelling is its own,
such as `42`, `-3` or `1.5`, is never reported, and neither is a param the
target declares as a number.

## Octal, and a leading zero

A plain scalar with a leading zero and octal digits, such as `0644`, is read
as octal, because that is how a file mode is written and how the first
Meridian targets read it. The integer is 420 and the leading zero is gone.
Quote a mode: `mode: "0644"`. The `unquoted-mode` lint carries the fix, and
on the six configuration management types the unquoted form is also a
compile error (`invalid-param`), so it never reaches a tool.

`08` and `09` are not octal. Older YAML libraries read them as the floats 8
and 9; Meridian reads them as text, since no number can have been meant.

## Dates and times

A plain `2026-11-18` stays the text `2026-11-18`. Some YAML libraries turn it
into a timestamp and write it back as `2026-11-18T00:00:00Z`, which is how a
date in a tag or a label changes shape between the document and the tool.
Meridian never does; the text the author wrote is what the tool gets.
`11:00` is text as well: nothing is read in base 60.

## Tags

`!!str` is the one YAML tag a document may carry; it makes a value text,
which quoting does as well. Every other tag is refused with its line and
column: `!!int "12"`, `!!float 1`, `!!bool yes`, `!!binary`, `!!set`, and
any local tag such as `!custom`. A tag that was ignored would read as
something other than what the author wrote (`!!int "12"` as the text `12`),
which is worse than a refusal.

## Indentation, tabs and duplicate keys

Indentation is meaning in block YAML, and a line indented one level too far
or too little is often still valid and means something else. Meridian's
reply is the one every tool has: an unknown field is an error at every
level except inside `params` and `metadata.labels`, so `dependsOn` indented
into `params` is reported as a param the target does not take, with its
line, and never silently kept. A tab in the indentation is an error, and so
is a key that appears twice in one mapping.

`meridian fmt` writes a document in one layout, two spaces a level, with
the language's own field order, and refuses to change anything that would
alter a value or lose a comment.

## Templating

The pitfall KYAML was mostly built for is text templating: a Helm chart
pastes a value into YAML text and the author must get the indentation right
outside the YAML. Meridian has no text templating. An
[interpolation](interpolation.md) `${{ ... }}` is decided on the parsed
value, before any target sees the document: `count: ${{ data.replicas }}`
is the number, `name: ${{ data.project }}-web` is the text, and a list or a
mapping placed as the whole value keeps its shape. The indentation of the
result is never the author's problem, and a value from data can never change
the structure of the document.

## Flow style, KYAML and JSON

A document may be written in block style, in YAML's flow style (`{a: 1}`,
`[1, 2]`), as [KYAML](https://kubernetes.io/docs/reference/encodings/kyaml/)
(a `---` header, one flow mapping, every string double-quoted, a trailing
comma after every entry) or as JSON; Meridian decides which from the content,
not the file name, and reads all of them with the same parser to the same
document. Tests pin that a KYAML document and its block-style twin parse the
same, and that `kubectl get -o kyaml` output imports the same as `-o yaml`.

`meridian fmt` normalises block layout only. A document written as one flow
mapping, as KYAML is, has no block layout to normalise and is left exactly as
written, like JSON. A flow collection inside a block document is kept as the
author wrote it. The linter reads a KYAML document, but a rule that points at
one value's text cannot place it inside a flow collection: `lossy-number`
reports inside block mappings only, and `unquoted-mode` reports a mode inside
a flow mapping at the document's first line, without a fix.

Meridian does not write KYAML, and does not plan to make it the input
format. KYAML is an output specification, and its own proposal keeps input
permissive. Meridian's scalar rules already remove the booleans problem,
interpolation removes the templating problem, and a second dialect of YAML
would be one more thing to document for no gain.

## What Meridian writes

Every target that writes YAML (Ansible, Salt, cloud-init, Kubernetes, Helm,
Docker Compose, CloudFormation, Pulumi, DSC) writes it through one writer,
in block style, with the same rules as the reference library the first
targets were written against. A string that a YAML 1.1 reader would take for
something else is quoted: `"no"`, `"yes"`, `"0644"`, `"08"`, `"1.10"`,
`"11:00"`, `"2026-11-18"`, `"12"`, `"null"`, and the keys `"no"` and `"on"`.
A number is written plainly and a multi-line string as a `|` literal block.
Mapping keys are written in a fixed order for each file, so the same document
always gives the same bytes.
