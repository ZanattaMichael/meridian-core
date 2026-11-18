# Error kinds and codes

Most of what Meridian refuses comes with a sentence that says what was wrong
and what to write instead. Behind each sentence is a *kind*: a fixed word that
says which sort of thing went wrong, so that a program using Meridian's crates
can act on the failure without reading its text, and so that this page can
list every sort there is.

A kind's code never changes and is never reused for another meaning. A new
sort of failure gets a new code; a code that stops being produced stays
reserved. The tests check that every code in the code is listed here.

This page lists the kinds by the crate that produces them. The messages
themselves are not listed: they are many, and each says what it needs to
where it appears. The [lint rules](lint.md) and the
[compile-rule refusals](compile-rules.md#refusals) have their own pages,
because each of those is a rule with a name of its own.

## Expressions (`meridian-expr`)

An expression or interpolation that cannot be read or evaluated. The error
carries the kind, the message and, where the text is known, the span of the
expression it points at, as byte offsets into the expression's source. In a
document the span is turned into a position after the field's own. The
[expressions reference](dsl/expressions.md#errors) shows an example message
for each.

| Code | When |
|---|---|
| `expr-syntax` | the text does not read as an expression, or ends before it is complete |
| `expr-limit` | the expression is nested more than 64 levels deep or has more than 1000 operators |
| `expr-unknown-name` | a name that nothing is called: not `data`, `var`, `cond` or `secret`, or a key none of them has |
| `expr-refused-name` | a name that is known but not allowed where it was written, such as `secret` in a condition |
| `expr-unknown-function` | a call to a function the language does not have |
| `expr-arity` | a function called with the wrong number of arguments |
| `expr-argument` | a function given an argument it cannot use |
| `expr-type` | an operator applied to values it does not work on, or a value that is not a yes or a no where one is needed |
| `expr-not-set` | a path that reads a value nothing has set |
| `expr-secret` | a secret could not be read, is not fetched here, or was combined with something it cannot be |
| `expr-interpolation` | an interpolation that is never closed, is empty, or whose value cannot be placed where it stands |
| `expr-other` | anything else that stops evaluation; a message that has no kind of its own yet |

The first five are found by `meridian validate`, with no data. The rest need
the values, so `compile`, `plan` and `apply` find them.

## Stages of a run (`meridian-orchestrator`)

Why a stage failed, as the run's record keeps it. The text the terminal shows
is the tool's or the plugin's; the record keeps only this word, because a tool
can quote a value. The same words appear in a [run's record](state.md#runsidrunjson)
and in the `failure` element of a [JUnit report](reporting.md).

| Code | When |
|---|---|
| `no-plugin` | no plugin provides the document's target |
| `compile-refused` | the document was refused before or while it was compiled |
| `not-supported` | the target cannot do what was asked, such as apply |
| `plugin-failed` | the plugin itself failed: its process died, or the call did |
| `tool-failed` | the tool said no: it exited non-zero, or could not reach the machine |
| `state-failed` | the step's working directory could not be prepared |
| `policy-denied` | the run's [policy](policy.md) does not allow the change, or could not be asked |

A step's record can also hold words that are not a stage failure: a step that
was skipped because an earlier one failed says `earlier-step-failed`, and a
rollback that could not put a step back has its own reasons, listed with the
[rollback events](state.md#events).

## Modules (`meridian-module`)

Which step of handling modules failed. The message names the module, the file
and what to do about it; the [modules reference](modules.md#when-something-goes-wrong)
explains the common ones.

| Code | When |
|---|---|
| `module-manifest` | `meridian.yaml` cannot be read, or says something it cannot: a bad name, version or source |
| `module-lock` | `meridian.lock` cannot be read, or does not match the manifest |
| `module-integrity` | a module's files are not the files its hash or its lock entry names |
| `module-registry` | a registry could not be reached, refused the request, or answered with something that is not a module |
| `module-git` | a git source could not be fetched or checked out |
| `module-pack` | a module archive could not be packed or unpacked: too large, or a path that leaves the module |
| `module-signature` | a module's signature is missing where it is required, or does not pass |
| `module-resolution` | the versions cannot be agreed on: a requirement no version meets, two requirements that conflict, or a module that needs another core |
| `module-install` | a resolved module could not be fetched and written into place, or an installed module is not the one the lock pins |
| `module-credential` | the credential a registry needs is not declared, or cannot be read |

## Compile rules (`meridian-rules`)

A mistake in the rules file, the hierarchy or one of its layers. This is never
a violation: a layer that breaks a rule is reported as a
[refusal](compile-rules.md#refusals) that lists every violation, not as an
error.

| Code | When |
|---|---|
| `rules-hierarchy` | the hierarchy file cannot be read, parsed or expanded |
| `rules-layer` | a layer the hierarchy names cannot be read, or its values cannot be merged |
| `rules-file` | the rules file is missing where it is required, cannot be read, or says something the rules language does not: an unknown rule, field, group or merge strategy |

## Layered data (`meridian-resolve`)

The resolver beneath the compile rules, which reads a hierarchy and merges its
layers. The rules crate turns these into its own kinds.

| Code | When |
|---|---|
| `resolve-hierarchy` | the hierarchy file does not parse, declares no layers, names a strategy that does not exist, or two of its entries resolve to the same layer |
| `resolve-layer` | a layer the hierarchy names could not be loaded |
| `resolve-merge` | two layers' values for one key could not be merged under its rule |

## Keys and signatures (`meridian-signing`)

Signing a file or a module and checking the signature, as
[plugin signatures](plugin-signatures.md) and
[module signatures](modules.md#signatures) do.

| Code | When |
|---|---|
| `signing-key` | a key file or key text is not a key of the right shape, or a secret key where a public one is needed |
| `signing-io` | a file could not be read, or the system's random source is not available |
| `signing-signature` | a signature does not pass, or the policy refuses a file that is unsigned |
| `signing-policy` | the word given for `--plugin-signatures` or `--module-signatures` is not `off`, `warn` or `require` |

## For plugin and tool authors

Each crate exports its `Kind` with a `code()` method and a `Kind::ALL` list,
and its `Error` with a `kind()` accessor. `meridian_orchestrator::Reason`
does the same for stages, with `as_str()`, and reads back from its word with
`str::parse`. An `Error` still displays as its message and converts into a
`String`, so code that only prints it needs no change.
