# Lint rules

Rules of [`meridian lint`](../tooling/linter.md), also shown by the
[language server](../tooling/language-server.md). Findings never stop compilation.

| Rule | Severity | Fix | What it finds |
|---|---|---|---|
| [`unquoted-mode`](#unquoted-mode) | warning | yes | a `mode` param written as a number |
| [`lossy-number`](#lossy-number) | warning | yes | a param holding a number that is not the one the author wrote |
| [`literal-secret`](#literal-secret) | warning | | a param that looks like a secret, holding a literal value |
| [`duplicate-resource`](#duplicate-resource) | warning | | two resources of one type that manage the same thing |
| [`redundant-dependency`](#redundant-dependency) | info | | a `dependsOn` entry that adds nothing |
| [`useless-if-missing`](#useless-if-missing) | warning | | `ifMissing: skip` on an edge to a resource that is always there |
| [`unguarded-exec`](#unguarded-exec) | info | | an `exec` that runs on every apply |
| [`unreferenceable-id`](#unreferenceable-id) | warning | | a resource id that cannot be used in a `${...}` reference |
| [`unsafe-document-name`](#unsafe-document-name) | warning | | a `metadata.name` with characters that do not belong in a file name |
| [`native-type`](#native-type) | info | | a resource using the tool's own type on a target that shares the six types |
| [`unused-resource-set-host`](#unused-resource-set-host) | info | | `targetHost` fields its `from` does not read |

Silence one with `# meridian-lint: disable=RULE`
([silencing a rule](../tooling/linter.md#silencing-a-rule)).

## `unquoted-mode`

**Warning, with a fix.** A `mode` param is written as a number.

```yaml
params:
  path: /etc/motd
  mode: 0644        # read as the number 644
```

YAML reads `0644` as 644. Quote it:

```yaml
  mode: "0644"
```

Compilation also refuses it (`invalid-param`, [resource types](resource-types.md)).
`--fix` adds the quotes.

## `lossy-number`

**Warning, with a fix.** A param holds a plain number whose spelling is not
what reaches the tool.

```yaml
params:
  image: nginx
  version: 1.10       # read as the number 1.1
  release: 3.0        # read as the number 3
  build: 1_000        # read as the number 1000
```

YAML reads `1.10` as 1.1, `3.0` as 3, `1_000` as 1000 and `+7` as 7. Quote it:

```yaml
  version: "1.10"
```

Only numbers whose canonical form differs are reported. Not checked: `mode` (see
[`unquoted-mode`](#unquoted-mode)), params declared numeric (such as `uid`), and flow
collections. `--fix` adds the quotes.

## `literal-secret`

**Warning.** A param that looks like a secret holds a literal value.

```yaml
params:
  name: deploy
  password: hunter2
```

Use `spec.secrets` and `${{ secret.<name> }}` instead.

Matched names contain `password`, `passwd`, `secret`, `token`, `api_key`, `apikey`,
`private_key` or `access_key` (any case), except `lock_*`, `*_file` and `*_path`. Empty values
and `${...}` references are not reported.

## `duplicate-resource`

**Warning.** Two resources of one type manage the same thing.

```yaml
- id: web_server
  type: package
  params: {name: nginx}
- id: proxy
  type: package
  params: {name: nginx}     # nginx is already managed by "web_server"
```

Compares `name`, else `path`, on fully described types that require it. Native types and
`${...}` values are not compared.

## `redundant-dependency`

**Info.** A `dependsOn` entry adds nothing.

```yaml
- id: service
  type: service
  params: {name: nginx}
  dependsOn:
    - resource: config
    - resource: package     # config already depends on package
```

The entry repeats another or is already implied through a chain. Entries with their own
`when` are neither reported nor counted.

## `useless-if-missing`

**Warning.** `ifMissing: skip` on an edge to a resource that is always there.

```yaml
dependsOn:
  - resource: package
    ifMissing: skip         # package has no `when`, so it is never missing
```

The named resource has no `when` ([conditions](dsl/conditions.md)), so `ifMissing` does
nothing. Checks `dependsOn` and `notifies`.

## `unguarded-exec`

**Info.** An `exec` runs on every apply.

```yaml
- id: migrate
  type: exec
  params:
    command: /opt/app/bin/migrate
```

No `creates`/`removes`, no `runtimeWhen`, and nothing notifies it. Add a guard or use
[`notifies`](dsl/notifies.md). Applies to the shared `exec` type.

## `unreferenceable-id`

**Warning.** A resource id cannot be used in a `${...}` reference.

```yaml
- id: web.server
  type: package
```

[References](dsl/references.md) allow only letters, digits, `_` and `-` in ids.

## `unsafe-document-name`

**Warning.** `metadata.name` holds characters that do not belong in a file
name.

```yaml
metadata:
  name: web servers/prod
```

Use letters, digits, `.`, `_` and `-`. `meridian validate` refuses others.

## `native-type`

**Info.** A resource uses the tool's own type on a target that shares the
six configuration management types.

```yaml
- id: workers
  type: ansible.builtin.lineinfile
  params:
    path: /etc/nginx/nginx.conf
    regexp: "^worker_processes"
    line: worker_processes 4;
```

A type such as `ansible.builtin.lineinfile` or `native/file`
([native types](dsl/configuration-management.md#native-types-beside-the-six)) ties the
document to one target. Marks where it stops being portable.

## `unused-resource-set-host`

**Info.** `targetHost` gives fields its `from` does not read.

```yaml
targetHost:
  from: inline
  host: web01.example.com
  ref: network            # not read with `from: inline`
```

`from: inline` reads only `host`; `from: infrastructure` only `ref` and `output`.
