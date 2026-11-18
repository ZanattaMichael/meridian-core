# Shared param kinds are checked once, strictly, for all six configuration management targets

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | `meridian_sdk::schema::check_shared`, called first by every configuration management compiler |
| Where it shows | [Resource types](../reference/resource-types.md), the [conformance matrix](../reference/conformance-matrix.md) |

## Context

The six configuration management targets (Ansible, Puppet, OpenVox, Chef, Salt and cloud-init) take the same resource types (`package`, `service`, `file`, `user`, `group`, `exec`), and each compiler checked the kinds of their params on its own. The checks disagreed: one target read `mode: 0644` as the number 420 and passed it on, another refused it; a lone name where a list of groups was wanted went through on some targets and not others; `yes` for a flag did the same. A document that compiled for one target could fail on another for a reason that had nothing to do with that tool (issue 89).

## The choice

The kinds of the shared params are checked in one place, `check_shared` in the SDK, which every one of the six compilers calls before its own rules. A wrong kind is refused as `invalid-param` with the same message on every target.

The reading is strict. Text where a name is wanted, true or false where a flag is wanted, a whole number for an id, a list where several are wanted. A number or a list where text is wanted, a string where a list is wanted, and `yes` or `"true"` where true is wanted are refused, each with a hint on how to write the value, rather than passed to the tool to interpret. An unquoted file mode such as `mode: 0644` is therefore a compile error as well as the `unquoted-mode` lint warning, which keeps its fix.

A secret stands for text, so it passes wherever text would. Two things stand aside from the kind check because the target has a rule that names the real problem: a param with no value at all (`empty-param`), and a secret inside a list or a map (`unsupported-secret-place`).

## Alternatives

Leaving each target to coerce what it can (`"true"` to true, a lone name to a one-item list, a whole float to an integer). Friendlier on one target, but each target would coerce differently and the matrix would stay inconsistent, which was the issue.

Checking the kinds in the IR, before any plugin runs. The IR does not know which types are shared or what their params mean; the SDK does, and a plugin for a new tool gets the same check by calling it.
