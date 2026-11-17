# The formatter

```
meridian fmt <paths>...            rewrite the files in place
meridian fmt --check <paths>...    change nothing; list the files that would change
meridian fmt --keep-order <paths>  format, but keep fields in the order written
meridian fmt -                     read standard input, write standard output
```

`meridian fmt` gives every document the same layout, so a diff shows what
changed and not how someone likes their YAML. It changes how a document is laid
out and **never what it says**.

A path may be a file or a directory. A directory is searched for `.yaml`,
`.yml` and `.json` files that are Meridian documents (an `apiVersion` starting
with `meridian/`); other YAML files, hidden directories, `node_modules` and
`target` are left alone. A file named outright that is not a Meridian
document is skipped, with a line saying so.

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
| Trailing spaces, and runs of blank lines (one is kept) | Quotes, and whether a value is written in block or flow style (`{name: nginx}` stays as it is) |
| A missing newline at the end of the file | Comments, which move with the field or list entry they are written above or beside |
| | Block scalars (`|` and `>`): their text is kept line for line |
| | JSON files, which are returned unchanged |

The field order is the one the language's own tables list, which is also the
order completion offers fields in. `--keep-order` turns reordering off and
does everything else.

## It cannot change your document

Two checks stand behind that. Before the formatter returns, the formatted text
is parsed again and compared, value for value, with the original, and its
comments are counted against the original's. If either differs the formatter
refuses, writes nothing and says so. A refusal is a bug in the formatter, not
something wrong with your file; please report it with the file.

A file that is not valid YAML is not formatted. The message is the compiler's.

## In CI

`--check` writes nothing, lists the files that are not formatted and exits `1`
if there are any:

```console
$ meridian fmt --check .
stacks/web.yaml
1 file not formatted; run `meridian fmt` to format
```

Every example under [`examples/`](../../examples) is formatted, and a test
keeps it so. Another test runs the formatter over every YAML file in the
repository and checks that none is refused and that formatting twice changes
nothing.

## In an editor

The language server formats with the same code, so Format Document and format
on save give the same result as the command. See
[the language server](language-server.md#formatting).
