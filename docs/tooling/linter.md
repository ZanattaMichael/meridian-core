# The linter

```
meridian lint <paths>... [--plugin-dir DIR] [--data FILE] [--set KEY=VALUE] [--state-dir DIR] [--fix] [--strict] [--json]
```

`meridian lint` checks a whole project the way the editor checks an open file.
It reports two kinds of thing:

- **Errors**: everything `meridian validate` and `meridian compile` would
  refuse. Each document is validated and then compiled by its target's real
  plugin, in memory. Nothing is written and no tool is run.
- **Lint findings**: things that compile and are probably not what you meant,
  such as a file mode that YAML read as a number. Each has a rule, listed in
  the [lint rule reference](../reference/lint.md).

A path may be a file or a directory. A directory is searched for `.yaml`,
`.yml` and `.json` files that are Meridian documents; hidden directories,
`node_modules` and `target` are skipped.

```console
$ meridian lint web.yaml
web.yaml:16:15: warning: mode 0644 is read as a number, not as permissions; write it as a quoted string [unquoted-mode]
web.yaml:28:21: info: "nginx" already comes first, because "site" depends on it [redundant-dependency]
web.yaml:29:11: info: exec "warm" has no `creates` or `removes` and nothing notifies it, so it runs on every apply [unguarded-exec]
web.yaml:33:9: error: resource type "exec" does not take param "colour" on target "ansible" (it takes: chdir, command, creates, removes, shell) [unsupported-param]
web.yaml:35:21: error: spec.resources[3].dependsOn[0].resource: resource "warm" depends on unknown resource "wbe" [invalid-document]
2 errors, 1 warning, 2 notes
```

Each line is `file:line:column: severity: message [code]`, which editors and CI
systems recognise.

## Severities and exit status

| Severity | Meaning | Exit status |
|---|---|---|
| `error` | the document does not compile | `1` |
| `warning` | it compiles and is very likely a mistake | `0`, or `1` with `--strict` |
| `info` | worth a look, often deliberate | `0` |

Use `--strict` in CI to fail on warnings as well. Notes never fail a run.

## Fixing

`--fix` applies the fixes that cannot change what a document means, and
reports the rest:

```console
$ meridian lint --fix web.yaml
...
fixed 1 finding
```

At present one rule has a fix: [`unquoted-mode`](../reference/lint.md#unquoted-mode)
adds the quotes.

## Silencing a rule

A finding that is deliberate can be silenced where it is, with a comment that
names the rule:

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

Several rules are separated by commas, and `all` stands for every rule. Only
lint findings can be silenced. An error is the compiler's, and the document
will not compile until it is fixed.

## Conditions and data

A document can read data: in a `when`, in `spec.conditions`, and in
`${{ ... }}` interpolation. What the linter can check depends on whether it
has that data.

- **A document that needs no data** (its conditions and interpolation read
  only `spec.variables` and literals) is worked out exactly as
  `meridian compile` would, and then compiled.
- **A document that needs data, with none given.** Every `when` is treated as
  true, so that a mistake in a resource that is usually switched off is still
  found. If the document also interpolates values with `${{ ... }}`, the
  values are not known, so it is checked against the language (fields, types,
  references, dependencies) and is not handed to the plugin.
- **With `--data FILE` or `--set KEY=VALUE`** (each may be repeated, and they
  mean what they mean for
  [`meridian compile`](../reference/cli.md#deciding-conditions)), conditions
  and interpolation are decided from the data. Resources that are switched off
  are not compiled, a document whose `spec.when` is false is not compiled, and a condition that cannot be decided is an `invalid-condition`
  error.

`--state-dir DIR` (or `MERIDIAN_STATE_DIR`, default `.meridian`) is where the
project's fetched modules are, for `--data` files named inside a module.

## Module manifests

A `meridian.yaml` with `kind: Module` describes a module and is not a document
to compile. `meridian lint` and `meridian fmt` leave it alone;
`meridian module` checks it.

## Plugins

The plugins are found as they are for every other command: `--plugin-dir`,
`MERIDIAN_PLUGIN_DIR`, and the directory `meridian` is in. A document whose
target has no plugin gets an `unknown-target` error.

## Files are checked one at a time

Each file is its own project. Two files in a directory may both hold a document
called `web`; they are alternatives, not a clash. A ResourceSet that takes its
host from an Infrastructure in another file under the same paths is checked
against that Infrastructure, nearest directory first.

## Machine-readable output

`--json` prints one JSON object per finding, one per line, and no summary:

```json
{"file":"web.yaml","line":16,"column":15,"endLine":16,"endColumn":19,"severity":"warning","code":"unquoted-mode","source":"meridian-lint","message":"mode 0644 is read as a number, not as permissions; write it as a quoted string","fixable":true}
```

`source` is `meridian-lint` for a lint rule, `meridian` for the language, and
the target's name for something its plugin said. Lines and columns start at 1.

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
