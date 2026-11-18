# A refusal's rule is one of a registry, and the same mistake has the same name on every target

| | |
|---|---|
| Area | Core |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | issue 88, one registry of refusal rule names |
| Where it shows | [Refusal rules](../reference/refusal-rules.md), [Conformance matrix](../reference/conformance-matrix.md), [`meridian plugins`](../reference/cli.md#plugins), [Writing a plugin](../contributing/writing-a-plugin.md#refusals) |

## Context

A `CompileError` carries a `rule`, documented as "a stable, machine-readable
name", but the name was a string written at each call site. Across the
eighteen plugins there were about seventy distinct names, and several said the
same thing differently: `unknown-param` beside `unsupported-param`,
`dangling-reference` beside `invalid-reference`, twenty-four kinds of
`duplicate-*`, and a `file` in state `directory` with `content` refused under
four different names on the six configuration management targets. No page
listed the rules. The MCP server (issue 122), `meridian test` (issue 114) and
analyzer findings (issue 116) all key off the rule, so a name that differs by
target or changes by accident breaks them.

## The choice

A rule is a value, not a string. `meridian_sdk::rules` holds the **general**
rules as constants, each with a name, a one-sentence meaning and a hint, and
`CompileError::for_resource` and `CompileError::at` take a `&Rule`, so a
plugin cannot refuse under a name the registry does not know. A mistake that
exists on one tool only gets a rule of that plugin's own, declared in one
table and reported through `Emitter::rules`, which `Describe` carries to the
host and `meridian plugins --rules` prints. A test fails when a plugin's own
rule reuses a general name, and the conformance suite fails when a refusal
carries a rule neither the registry nor the target declares.

The near-duplicates were folded into one name each. The folds that change what
a user sees:

| Before | Now |
|---|---|
| `unknown-param` | `unsupported-param` |
| `unknown-reference`, `dangling-reference`, `unsupported-reference`, `self-reference` | `invalid-reference` |
| `unknown-notifier`, `missing-handler`, `unbound-reaction` | `unknown-notify-target` |
| every `duplicate-<thing>` but `duplicate-output` | `duplicate-resource` |
| `conflicting-engine` | `conflicting-settings` |
| `reserved-text`, `reserved-param`, `reserved-sls-name`, `reserved-id` | `reserved-name` |
| `relative-path`, `unsafe-path` | `invalid-path` |
| `invalid-<deployment, stack, release, chart, cookbook, class, sls>-name`, `empty-title`, `multiline-*`, `quoted-title`, `empty-id`, `empty-name` | `invalid-name` |
| `invalid-owner`, `invalid-mode`, `invalid-encoding`, `invalid-command`, `unrepresentable-value` | `invalid-param` |
| `empty-template`, `empty-project`, `empty-configuration`, `empty-chart`, `empty-manifests`, `empty-cookbook` | `empty-document` |
| `missing-deployment` | `missing-resource` |
| `missing-source`, `missing-name`, `missing-function`, `missing-module`, `empty-resource` | `missing-param` |
| `missing-requisite` | `unknown-dependency` |
| `self-requisite`, `reference-cycle`, `cycle` | `dependency-cycle` |
| `conditional-handler`, `conditional-reaction` | `unsupported-runtime-condition` |

Four rules stay a target's own, because the mistake exists on one tool:
`shell-required` (Salt), `unsatisfiable-order` (cloud-init), `unnamed-secret`
(Puppet), `invalid-expression` and `template-too-large` (ARM).

A `file` in state `directory` with `content` is now `param-conflicts-with-state`
on all six configuration management targets, and the conformance matrix marks
the row *same*.

`CompileError` gains a `hint`: what to write instead, as data, filled from the
rule unless the refusal gives a more specific one. `meridian` prints it after
the message as `(hint: ...)`, and it crosses the wire in a new field, so an
older plugin simply sends none.

The reference page [Refusal rules](../reference/refusal-rules.md) is
generated from the registry and the plugins, like the capability and
conformance matrices, so a rule cannot exist in the code without being on it.

## Alternatives

- **Keep strings and add a test that lists the allowed names.** Nothing stops
  a new plugin from inventing a name until the test is updated, and the
  reference page would be a second list to keep in step. A `&Rule` argument
  makes the compiler the check.
- **One rule per plugin for every mistake, namespaced by target.** It would
  defeat the point: the shared types are meant to be common, and a script
  asserting "refused as `unsupported-param`" must not have to know the target.
- **Keep the fine-grained `duplicate-*` names.** They told a reader *what* was
  duplicated, but the message already says that, and a consumer wanting "a
  second one of something" had twenty-four names to match.

## Consequences

Renaming a rule is a visible change, which is why it is done before the first
release of the plugin protocol is relied on. A name on this page is now a
compatibility surface: it is not reused for another meaning, and a rule that
stops being produced stays reserved, as [error codes](0026-error-codes-are-fixed-words-and-never-reused.md)
do. A plugin outside this repository that refuses with a name the registry
does not know still compiles against an older SDK, and its refusal is shown as
it sends it.
