# The linter

```
meridian lint <paths>... [--plugin-dir DIR] [--data FILE] [--set KEY=VALUE] [--state-dir DIR] [--fix] [--strict] [--json]
```

`meridian lint` checks a whole project the way the editor checks an open file.
It reports two kinds of finding:

- **Errors**: everything `meridian validate` and `meridian compile` would
  refuse. Each document is validated, then compiled in memory by its target's
  real plugin. Nothing is written and no tool runs.
- **Lint findings**: things that compile but are probably mistakes, such as a
  file mode YAML read as a number. Each has a rule, listed in
  the [lint rule reference](../reference/lint.md).

A path may be a file or directory. Directories are searched for `.yaml`, `.yml`
and `.json` Meridian documents; hidden directories, `node_modules` and `target`
are skipped.

```console
$ meridian lint web.yaml
web.yaml:16:9: error: param "mode" of a file must be text, not a number; write it quoted, so it is not read as a number: "420" [invalid-param]
web.yaml:16:15: warning: mode 0644 is read as a number, not as permissions; write it as a quoted string [unquoted-mode]
web.yaml:28:21: info: "nginx" already comes first, because "site" depends on it [redundant-dependency]
web.yaml:29:11: info: exec "warm" has no `creates` or `removes` and nothing notifies it, so it runs on every apply [unguarded-exec]
web.yaml:33:9: error: resource type "exec" does not take param "colour" on target "ansible" (it takes: chdir, command, creates, removes, shell) [unsupported-param]
web.yaml:35:21: error: spec.resources[3].dependsOn[0].resource: resource "warm" depends on unknown resource "wbe" [invalid-document]
3 errors, 1 warning, 2 notes
```

Each line is `file:line:column: severity: message [code]`, which editors and CI
systems recognise.

## Severities and exit status

| Severity | Meaning | Exit status |
|---|---|---|
| `error` | the document does not compile | `1` |
| `warning` | it compiles and is very likely a mistake | `0`, or `1` with `--strict` |
| `info` | worth a look, often deliberate | `0` |

Use `--strict` in CI to also fail on warnings. Notes never fail a run.

## Fixing

`--fix` applies the fixes that cannot change what a document means, and
reports the rest:

```console
$ meridian lint --fix web.yaml
...
fixed 1 finding
```

Two rules have fixes: [`unquoted-mode`](../reference/lint.md#unquoted-mode) and
[`lossy-number`](../reference/lint.md#lossy-number) both quote a number meant as
text. The `unquoted-mode` fix also clears the plugin's `invalid-param` error on
the same line, since both describe one mistake.

## Silencing a rule

Silence a deliberate finding in place with a comment naming the rule:

```yaml
params:
  # meridian-lint: disable=literal-secret
  password: not-a-real-one

  token: also-fine   # meridian-lint: disable=literal-secret
```

| Comment | Covers |
|---|---|
| `# meridian-lint: disable=RULE` on a line of its own | the line below it |
| `# meridian-lint: disable=RULE` at the end of a line | that line |
| `# meridian-lint: disable-file=RULE` anywhere in the file | the whole file |

Separate several rules with commas; `all` means every rule. Only lint findings
can be silenced. Errors come from the compiler and must be fixed.

## Conditions and data

A document can read data in a `when`, in `spec.conditions`, and in
`${{ ... }}` interpolation. What the linter can check depends on whether it has
that data.

- **No data needed** (conditions and interpolation read only `spec.variables`
  and literals): evaluated and compiled exactly as `meridian compile` would.
- **Data needed but not given:** every `when` is treated as true, so mistakes
  in usually-disabled resources are still found. If the document also
  interpolates `${{ ... }}`, the values are unknown, so it is checked against
  the language only (fields, types, references, dependencies), not passed to
  the plugin.
- **With `--data FILE` or `--set KEY=VALUE`** (repeatable, as for
  [`meridian compile`](../reference/cli.md#deciding-conditions)): conditions and
  interpolation are decided from the data. Disabled resources and documents
  whose `spec.when` is false are not compiled. An undecidable condition is an
  `invalid-condition` error.

`--state-dir DIR` (or `MERIDIAN_STATE_DIR`, default `.meridian`) holds the
project's fetched modules (for `--data` files inside a module) and the
[tool schemas](schema-sync.md) cached by `meridian schema sync`, so a tool's own
params are checked like a plugin's.

## Module manifests and tests

A `meridian.yaml` with `kind: Module` describes a module, not a document to
compile. `meridian lint` and `meridian fmt` skip it; `meridian module` checks it.
`kind: Test` documents are skipped too; [`meridian test`](../reference/testing.md)
reads them and refuses mistakes before any case runs.

## Plugins

Plugins are found as for every other command: `--plugin-dir`,
`MERIDIAN_PLUGIN_DIR`, and `meridian`'s own directory. A document whose target
has no plugin gets an `unknown-target` error.

## Files are checked one at a time

Each file is its own project. Two files may both hold a document called `web`;
they are alternatives, not a clash. A ResourceSet that takes its host from an
Infrastructure in another file under the same paths is checked against it,
nearest directory first.

## Machine-readable output

`--json` prints one JSON object per finding, one per line, and no summary:

```json
{"file":"web.yaml","line":16,"column":15,"endLine":16,"endColumn":19,"severity":"warning","code":"unquoted-mode","source":"meridian-lint","message":"mode 0644 is read as a number, not as permissions; write it as a quoted string","fixable":true}
```

`source` is `meridian-lint` for a lint rule, `meridian` for the language, or the
target's name for a plugin finding. Lines and columns start at 1.

## In CI

```yaml
- name: Meridian documents are formatted and clean
  run: |
    meridian fmt --check .
    meridian lint --strict .
```

## In an editor

The language server shows the same findings as you type, with the same quick
fixes. See [the language server](language-server.md#errors-and-warnings).
