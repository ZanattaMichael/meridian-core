# The JSON Schema

```
meridian schema [--target TARGET] [-o FILE] [--plugin-dir DIR]
```

`meridian schema` writes a [JSON Schema](https://json-schema.org/) (draft 7)
of the document language. It is for editors and tools that validate YAML
against a schema and cannot run a language server, and for anything else that
wants a machine-readable description of a Meridian document.

If your editor can run the [language server](language-server.md), use that
instead. It knows everything the schema does and a good deal the schema cannot
say.

## The published schema

The schema for the language and every target in this repository is checked in
at [`schemas/meridian-v1.schema.json`](../../schemas/meridian-v1.schema.json),
and a test fails when it is out of date. Point an editor at:

```
https://raw.githubusercontent.com/ZanattaMichael/meridian-core/main/schemas/meridian-v1.schema.json
```

To get a schema for exactly the plugins you have installed, generate one:

```sh
meridian schema -o meridian.schema.json
```

`--target ansible` writes the rules for that one target and leaves the others
out, which makes a smaller schema for a project that uses one tool.

## Using it

With [yaml-language-server](https://github.com/redhat-developer/yaml-language-server)
(the YAML support in VS Code, Neovim, Helix and others), name the schema in a
comment at the top of a document:

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/ZanattaMichael/meridian-core/main/schemas/meridian-v1.schema.json
apiVersion: meridian/v1
kind: ResourceSet
```

or map it to files in the editor's settings. In VS Code with the YAML
extension:

```json
{
  "yaml.schemas": {
    "./meridian.schema.json": ["stacks/**/*.yaml"]
  }
}
```

JetBrains IDEs take the same file under *Languages & Frameworks, Schemas and
DTDs, JSON Schema Mappings*.

Do not use both the schema and the Meridian language server on the same files:
each problem would be reported twice.

## What it checks

The schema is generated from the tables the parser reads and from what the
loaded plugins declare. It is never written by hand.

| It checks | From |
|---|---|
| The fields a document, a resource, a dependency and every other mapping may have, which are required, and that no others are present | the language's field tables |
| The values of `apiVersion`, `kind`, `ifMissing`, `targetHost.from` and other fields with fixed choices, each with its description | the language's field tables |
| That `target` is a target that exists | the loaded plugins |
| That a resource's `type` is one its target has, by name or by shape (`<provider>_<type>` becomes a pattern) | each plugin |
| For targets that describe their types (ansible, puppet, openvox, chef, salt, cloud-init): the params a type takes, which are required, their kinds and fixed values, and the states the type takes | each plugin |

Every field carries its documentation as a `description`, which editors show
on hover.

## What it cannot check

A schema describes the shape of one document. It cannot follow a name, so it
does not know that:

- a `dependsOn` or `notifies` names a resource that exists, or that the
  dependencies have no cycle;
- a `${...}` reference names something real;
- a target accepts this kind of document (`kind: Infrastructure` with a
  configuration management target);
- a `when` condition or a `${{ ... }}` expression is written correctly, or
  names a variable, condition or secret the document declares;
- a plugin refuses a combination of params that are each allowed.

`meridian validate`, `meridian lint` and the language server check all of
these. A document that passes the schema can still fail to compile; a document
the compiler accepts always passes the schema. Tests hold both directions for
every example in this repository: each example is valid against the schema,
and a set of documents the compiler refuses are refused by the schema too.

Two details follow from YAML. A param may always be a string, because any
param can hold a `${...}` reference. And `targetVersion: 3` is accepted as
well as `"3"`, because the compiler reads either as text.
