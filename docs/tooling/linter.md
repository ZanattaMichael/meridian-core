# The linter

```
meridian lint <paths>... [--plugin-dir DIR] [--data FILE] [--set KEY=VALUE] [--state-dir DIR] [--fix] [--strict] [--json]
```

`meridian lint` checks a project as the editor checks a file:

- **Errors**: what `validate` and `compile` refuse, compiled in memory by the real plugin.
- **Lint findings**: likely mistakes that compile ([rules](../reference/lint.md)).

Directories are searched as for [`meridian fmt`](formatter.md).

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

Each line is `file:line:column: severity: message [code]`.

## Severities and exit status

| Severity | Meaning | Exit status |
|---|---|---|
| `error` | the document does not compile | `1` |
| `warning` | it compiles and is very likely a mistake | `0`, or `1` with `--strict` |
| `info` | worth a look, often deliberate | `0` |

`--strict` also fails on warnings. Notes never fail.

## Fixing

`--fix` applies meaning-preserving fixes and reports the rest:

```console
$ meridian lint --fix web.yaml
...
fixed 1 finding
```

[`unquoted-mode`](../reference/lint.md#unquoted-mode) and
[`lossy-number`](../reference/lint.md#lossy-number) have fixes; the first also clears the
same line's `invalid-param` error.

## Silencing a rule

Silence a finding with a comment:

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

Separate rules with commas; `all` means every rule. Errors cannot be silenced.

## Conditions and data

- **No data needed:** compiled as `meridian compile` would.
- **Data needed, not given:** every `when` is true; with `${{ ... }}` the document is
  checked against the language only.
- **`--data FILE` or `--set KEY=VALUE`** ([as for compile](../reference/cli.md#deciding-conditions)):
  decided from the data; an undecidable condition is `invalid-condition`.

`--state-dir` (default `.meridian`) supplies fetched modules and cached
[tool schemas](schema-sync.md).

## Module manifests and tests

`kind: Module` and `kind: Test` files are skipped; `meridian module` and
[`meridian test`](../reference/testing.md) check them.

## Plugins

Found via `--plugin-dir`, `MERIDIAN_PLUGIN_DIR` and `meridian`'s directory; a missing one is
`unknown-target`.

## Files are checked one at a time

Two files may both define `web`. A ResourceSet whose host comes from another file's
Infrastructure is checked against the nearest one.

## Machine-readable output

`--json` prints one object per line:

```json
{"file":"web.yaml","line":16,"column":15,"endLine":16,"endColumn":19,"severity":"warning","code":"unquoted-mode","source":"meridian-lint","message":"mode 0644 is read as a number, not as permissions; write it as a quoted string","fixable":true}
```

`source` is `meridian-lint`, `meridian`, or the target. Lines and columns start at 1.

## In CI

```yaml
- name: Meridian documents are formatted and clean
  run: |
    meridian fmt --check .
    meridian lint --strict .
```

## In an editor

The [language server](language-server.md#errors-and-warnings) shows the same findings.
