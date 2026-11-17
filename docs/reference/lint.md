# Lint rules

The rules of [`meridian lint`](../tooling/linter.md), which the
[language server](../tooling/language-server.md) also shows as you type. A
lint finding is never an error: the document compiles. It points at something
that is allowed and likely to hurt later.

| Rule | Severity | Fix | What it finds |
|---|---|---|---|
| [`unquoted-mode`](#unquoted-mode) | warning | yes | a `mode` param written as a number |
| [`literal-secret`](#literal-secret) | warning | | a param that looks like a secret, holding a literal value |
| [`duplicate-resource`](#duplicate-resource) | warning | | two resources of one type that manage the same thing |
| [`redundant-dependency`](#redundant-dependency) | info | | a `dependsOn` entry that adds nothing |
| [`useless-if-missing`](#useless-if-missing) | warning | | `ifMissing: skip` on an edge to a resource that is always there |
| [`unguarded-exec`](#unguarded-exec) | info | | an `exec` that runs on every apply |
| [`unreferenceable-id`](#unreferenceable-id) | warning | | a resource id that cannot be used in a `${...}` reference |
| [`unsafe-document-name`](#unsafe-document-name) | warning | | a `metadata.name` with characters that do not belong in a file name |
| [`unused-resource-set-host`](#unused-resource-set-host) | info | | `targetHost` fields its `from` does not read |

Any rule can be silenced where a finding is deliberate, with a
`# meridian-lint: disable=RULE` comment. See
[silencing a rule](../tooling/linter.md#silencing-a-rule).

A test (`crates/meridian-tooling/tests/lint.rs`) fails if a rule exists
without a section on this page, so the page cannot fall behind the linter.

## `unquoted-mode`

**Warning, with a fix.** A `mode` param is written as a number.

```yaml
params:
  path: /etc/motd
  mode: 0644        # read as the number 644
```

YAML reads `0644` as the number 644, not as the permissions `0644`, and the
leading zero is lost before any tool sees it. Write the mode as a quoted
string:

```yaml
  mode: "0644"
```

`meridian lint --fix`, and the quick fix in an editor, add the quotes around
exactly what was typed.

## `literal-secret`

**Warning.** A param that looks like a secret holds a literal value.

```yaml
params:
  name: deploy
  password: hunter2
```

A password, token or key written into a document ends up in version control
and in the compiled output. Declare it under `spec.secrets` and write
`${{ secret.<name> }}` where it is used, reference another resource's value,
or supply it at apply time through the tool's own secret mechanism.

A param looks like a secret when its name contains `password`, `passwd`,
`secret`, `token`, `api_key`, `apikey`, `private_key` or `access_key`, in any
letter case. Names that begin with `lock_` or end with `_file` or `_path` are
settings about a secret rather than the secret, and are left alone. A value
that contains a `${...}` reference is not a literal, and neither is an empty
one.

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

Two `package` resources named `nginx`, or two `file` resources with one path,
give the tool two answers for one thing. Most tools refuse this only at apply
time, and some apply whichever comes last.

The rule compares the `name` param, or else the `path` param, of resources of
the same type. It only applies to types whose target describes them fully and
requires that param, so it never guesses about a native type such as
`aws_instance`. Values that contain a `${...}` reference are not compared.

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

The entry repeats another, or names a resource that one of the other
dependencies already depends on, directly or through a chain. The order is the
same without it. This is a note and not a warning because some teams list
every dependency on purpose.

An entry with its own `when` is never reported, and is not counted as a path
either: it may be switched off, so it does not make another entry redundant.

## `useless-if-missing`

**Warning.** `ifMissing: skip` on an edge to a resource that is always there.

```yaml
dependsOn:
  - resource: package
    ifMissing: skip         # package has no `when`, so it is never missing
```

`ifMissing` only matters when a `when` condition can remove the resource the
edge names ([conditions](dsl/conditions.md)). That resource has no `when`, so
the edge can never be missing and the policy does nothing. It usually means the
`when` was meant to be on the other resource, or was removed and this was left
behind. The rule checks `dependsOn` and `notifies` entries alike.

## `unguarded-exec`

**Info.** An `exec` runs on every apply.

```yaml
- id: migrate
  type: exec
  params:
    command: /opt/app/bin/migrate
```

The command has no `creates` or `removes` guard, no `runtimeWhen`, and nothing
notifies it, so it runs every time the document is applied and the plan is
never in sync. Add a guard, or trigger it from the resource whose change it
follows with [`notifies`](dsl/notifies.md).

The rule applies to the shared `exec` type on targets that describe it with a
`creates` param.

## `unreferenceable-id`

**Warning.** A resource id cannot be used in a `${...}` reference.

```yaml
- id: web.server
  type: package
```

A [reference](dsl/references.md) names a resource by letters, digits, `_` and
`-`. An id with a dot, a space or anything else cannot be written as
`${id.param}`, because the reference would be read as a different path.

## `unsafe-document-name`

**Warning.** `metadata.name` holds characters that do not belong in a file
name.

```yaml
metadata:
  name: web servers/prod
```

The name becomes a directory under `--out` and under the state directory. Keep
it to letters, digits, `.`, `_` and `-`.

## `unused-resource-set-host`

**Info.** `targetHost` gives fields its `from` does not read.

```yaml
targetHost:
  from: inline
  host: web01.example.com
  ref: network            # not read with `from: inline`
```

With `from: inline` only `host` is read; with `from: infrastructure` only `ref`
and `output` are. The other fields are ignored, which usually means `from` is
not what was intended.
