# The formatter

```
meridian fmt <paths>...            rewrite the files in place
meridian fmt --check <paths>...    change nothing; list the files that would change
meridian fmt --keep-order <paths>  format, but keep fields in the order written
meridian fmt -                     read standard input, write standard output
```

`meridian fmt` gives every document the same layout, so diffs show real
changes, not style. It changes layout and **never meaning**.

A path may be a file or directory. Directories are searched for `.yaml`, `.yml`
and `.json` Meridian documents (an `apiVersion` starting with `meridian/`).
Other YAML files, hidden directories, `node_modules` and `target` are ignored.
A named file that is not a Meridian document is skipped with a note.

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

Fields follow the order in the language's tables, the same order completion
uses. `--keep-order` turns off reordering and keeps everything else.

## It cannot change your document

Two checks guarantee this. The formatted text is parsed again and compared value
for value with the original, and its comments are counted against the
original's. If either differs, the formatter refuses and writes nothing. That
is a formatter bug, not a problem with your file; please report it with the
file.

A file that is not valid YAML is not formatted. The message is the compiler's.

## In CI

`--check` writes nothing, lists unformatted files, and exits `1` if there are
any:

```console
$ meridian fmt --check .
stacks/web.yaml
1 file not formatted; run `meridian fmt` to format
```

A test keeps every example under [`examples/`](../../examples) formatted.
Another formats every YAML file in the repository and checks none is refused
and a second pass changes nothing.

## In an editor

The language server uses the same code, so Format Document and format on save
match the command. See
[the language server](language-server.md#formatting).
