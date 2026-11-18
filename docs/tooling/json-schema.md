# The JSON Schema

```
meridian schema [--target TARGET] [-o FILE] [--plugin-dir DIR]
```

`meridian schema` writes a [JSON Schema](https://json-schema.org/) (draft 7) of the
language, for editors without the [language server](language-server.md), which checks
more.

## The published schema

[`schemas/meridian-v1.schema.json`](../../schemas/meridian-v1.schema.json) covers every target
in this repository and is kept current by a test:

```
https://raw.githubusercontent.com/ZanattaMichael/meridian-core/main/schemas/meridian-v1.schema.json
```

For your installed plugins:

```sh
meridian schema -o meridian.schema.json
```

`--target ansible` limits it to one target.

## Using it

With [yaml-language-server](https://github.com/redhat-developer/yaml-language-server), add a
comment:

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/ZanattaMichael/meridian-core/main/schemas/meridian-v1.schema.json
apiVersion: meridian/v1
kind: ResourceSet
```

or map it in settings. VS Code:

```json
{
  "yaml.schemas": {
    "./meridian.schema.json": ["stacks/**/*.yaml"]
  }
}
```

JetBrains: *Languages & Frameworks, Schemas and DTDs, JSON Schema Mappings*. Do not combine
it with the language server; problems would show twice.

## What it checks

Generated from the parser's tables and the plugins.

| It checks | From |
|---|---|
| The fields a document, a resource, a dependency and every other mapping may have, which are required, and that no others are present | the language's field tables |
| The values of `apiVersion`, `kind`, `ifMissing`, `targetHost.from` and other fields with fixed choices, each with its description | the language's field tables |
| That `target` is a target that exists | the loaded plugins |
| That a resource's `type` is one its target has, by name or by shape (`<provider>_<type>` becomes a pattern) | each plugin |
| For targets that describe their types (ansible, puppet, openvox, chef, salt, cloud-init): the params a type takes, which are required, their kinds and fixed values, and the states the type takes | each plugin |
| For a tool's own types, once `meridian schema sync` has run: the same, nested params included, with a param the tool sets left out so a document cannot write it | the [synced tool schemas](schema-sync.md) under `--state-dir` |

Each field has a `description` for hover.

## What it cannot check

It cannot check:

- that `dependsOn` and `notifies` names exist, or cycles;
- that `${...}` references resolve;
- that a target accepts the document's kind;
- `when` and `${{ ... }}` expressions;
- param combinations a plugin refuses.

`meridian validate`, `meridian lint` and the language server do. Anything the compiler
accepts passes the schema; tests check both directions on the examples.

Any param may be a string (for references), and `targetVersion: 3` equals `"3"`.
