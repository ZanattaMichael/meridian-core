# Error kinds and codes

Each error has a *kind*, a fixed code programs can match. Codes never change or get reused;
tests check this page lists them all. See also [lint rules](lint.md),
[compile-rule refusals](compile-rules.md#refusals) and [refusal rules](refusal-rules.md).

## Expressions (`meridian-expr`)

Errors carry the kind, message and byte span. See
[examples](dsl/expressions.md#errors).

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

The first five are found by `validate`; the rest need data.

## Stages of a run (`meridian-orchestrator`)

The word a [run's record](state.md#runsidrunjson) and [JUnit](reporting.md) `failure` keep
for a failed stage.

| Code | When |
|---|---|
| `no-plugin` | no plugin provides the document's target |
| `compile-refused` | the document was refused before or while it was compiled |
| `not-supported` | the target cannot do what was asked, such as apply |
| `plugin-failed` | the plugin itself failed: its process died, or the call did |
| `tool-failed` | the tool said no: it exited non-zero, or could not reach the machine |
| `state-failed` | the step's working directory could not be prepared |
| `policy-denied` | the run's [policy](policy.md) does not allow the change, or could not be asked |
| `interrupted` | the operator stopped the run with Ctrl-C (or a `SIGTERM`) while the step was going, or before its tool was started |
| `timed-out` | the step's tool ran past `--stage-timeout` and was stopped |
| `analysis-failed` | an [analyzer](analyzers.md) found something at or above the severity the run fails on (`--fail-on`) |
| `analyzer-failed` | an analyzer could not be run, or did not answer, so the step was not analyzed |
| `plan-differs` | an `apply --plan` compiled the stage to an artifact other than the one the saved plan recorded, the plan previewed a stage the document now leaves out, the tool (saving no plan) would now make other changes than the plan showed, or it cannot say what it would change and `--accept-unknown` was not given; the stages after it are not run |

Also: `earlier-step-failed` for skipped steps, and rollback reasons under
[events](state.md#events).

## Modules (`meridian-module`)

See [when something goes wrong](modules.md#when-something-goes-wrong).

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

Mistakes in the rules file or hierarchy. Violations are [refusals](compile-rules.md#refusals).

| Code | When |
|---|---|
| `rules-hierarchy` | the hierarchy file cannot be read, parsed or expanded |
| `rules-layer` | a layer the hierarchy names cannot be read, or its values cannot be merged |
| `rules-file` | the rules file is missing where it is required, cannot be read, or says something the rules language does not: an unknown rule, field, group or merge strategy |

## Layered data (`meridian-resolve`)

The hierarchy resolver; wrapped by the rules crate.

| Code | When |
|---|---|
| `resolve-hierarchy` | the hierarchy file does not parse, declares no layers, names a strategy that does not exist, or two of its entries resolve to the same layer |
| `resolve-layer` | a layer the hierarchy names could not be loaded |
| `resolve-merge` | two layers' values for one key could not be merged under its rule |

## Keys and signatures (`meridian-signing`)

For [plugin](plugin-signatures.md) and [module](modules.md#signatures) signatures.

| Code | When |
|---|---|
| `signing-key` | a key file or key text is not a key of the right shape, or a secret key where a public one is needed |
| `signing-io` | a file could not be read, or the system's random source is not available |
| `signing-signature` | a signature does not pass, or the policy refuses a file that is unsigned |
| `signing-policy` | the word given for `--plugin-signatures` or `--module-signatures` is not `off`, `warn` or `require` |

## For plugin and tool authors

Each crate exports `Kind` (`code()`, `Kind::ALL`) and `Error` (`kind()`, displays as the
message, converts to `String`). `meridian_orchestrator::Reason` has `as_str()` and
`str::parse`.
