# The formatter

```
meridian fmt <paths>...            rewrite the files in place
meridian fmt --check <paths>...    change nothing; list the files that would change
meridian fmt --keep-order <paths>  format, but keep fields in the order written
meridian fmt -                     read standard input, write standard output
```

`meridian fmt` normalises layout and **never meaning**. Directories are searched for
`.yaml`, `.yml` and `.json` files with an `apiVersion` starting `meridian/`; hidden
directories, `node_modules` and `target` are skipped.

## What it does

Before:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
    name: web
spec:
  target: ansible
  resources:
    - type: package
      id: nginx
      params: {name: nginx}
    - id: site
      type: file
      dependsOn:
      - resource: nginx
      params:
        path: /etc/nginx/conf.d/site.conf
```

After:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  resources:
    - id: nginx
      type: package
      params: {name: nginx}
    - id: site
      type: file
      params:
        path: /etc/nginx/conf.d/site.conf
      dependsOn:
        - resource: nginx
```

| It changes | It leaves alone |
|---|---|
| Indentation: two spaces, with list items indented under their key | Every value, exactly as typed, including numbers such as `0644` |
| The order of the language's own fields: `apiVersion`, `kind`, `metadata`, `spec`; in `spec` `target`, `targetVersion`, `when`, `variables`, `conditions`, `secrets`, `targetHost`, `imports`, `resources`, `outputs`; in a resource `id`, `type`, `state`, `when`, `runtimeWhen`, `params`, `dependsOn`, `notifies` | The order of anything the language does not define: params, labels, resources, list entries |
| Trailing spaces, and runs of blank lines (one is kept), outside block scalars | Quotes, and whether a value is written in block or flow style (`{name: nginx}` stays as it is) |
| A missing newline at the end of the file | Comments, which move with the field or list entry they are written above or beside |
| A byte order mark at the start of the file, which is dropped | Block scalars (`|` and `>`): their text is kept line for line, trailing spaces included, and a `|+` or `>+` at the end of the file keeps the blank lines after it, which are part of its value |
| | JSON files, which are returned unchanged |

Fields follow the language's order. `--keep-order` keeps yours.

## It cannot change your document

The result is re-parsed and compared value for value, and comments are counted. On any
difference nothing is written; please report it. Invalid YAML is not formatted.

## In CI

`--check` lists unformatted files and exits `1`:

```console
$ meridian fmt --check .
stacks/web.yaml
1 file not formatted; run `meridian fmt` to format
```

Tests keep [`examples/`](../../examples) formatted and check formatting is idempotent.

## In an editor

The [language server](language-server.md#formatting) uses the same code.
