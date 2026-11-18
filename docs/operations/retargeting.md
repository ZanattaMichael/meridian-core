# Moving a document to another tool

<p align="center"><img src="../graphics/meridian-retarget.svg" alt="retarget: the old tool reports the identifier of each resource it manages, meridian retarget writes a new file with the new target and an adopt line per resource, and the new tool's first plan adopts each one instead of creating it" width="100%"></p>

`meridian retarget` moves a document from one target to another, such as
Terraform to OpenTofu, without creating again what the first tool already
made:

```
meridian retarget site.yaml --to opentofu
```

```
network: terraform -> opentofu, 3 resources adopted
wrote site.opentofu.yaml; check it with `meridian plan site.opentofu.yaml`
```

It does three things:

1. Asks the old tool for the identifier of each resource it manages, from
   the state its last `meridian apply` left in the state directory.
2. Writes a new file with `spec.target` changed and, under each of those
   resources, an [`adopt:`](../reference/dsl/resources.md#adopting-what-exists)
   line holding the identifier.
3. Leaves the file it read as it was.

Then plan the new file. Each adopted resource shows as `= adopt`, not
`+ create`:

```
meridian plan site.opentofu.yaml
```

## What depends on the new target

| New target adopts by | What `retarget` writes |
|---|---|
| `import` (Terraform, OpenTofu, Pulumi) | `adopt:` for each resource the old tool manages |
| `converge` (Ansible, Puppet, Kubernetes, Bicep and the others in the [capability matrix](../reference/capability-matrix.md)) | only the new `spec.target`: the tool makes what exists match the document |
| no | only the new `spec.target`, and a warning that the first apply creates every resource again |

## Which old tools can be read

The old target's plugin must say it reads identifiers. Terraform and OpenTofu
read `terraform show -json` (the resource's `id`), and Pulumi reads
`pulumi stack export`. For any other old target, give the identifiers as
`adopt:` by hand, or move the document with `--no-adopt`.

The `id` a tool reports is the import identifier for most types. Where a
provider documents another import format for a type, correct that `adopt:`
line before the apply.

## Options

| Option | |
|---|---|
| `--to TARGET` | the target to move to; its plugin must be installed |
| `--out FILE` | where to write. See [Where the moved file goes](#where-the-moved-file-goes) |
| `--no-adopt` | read nothing from the old tool and write no `adopt:` lines |
| `--state-dir DIR` | where the old tool's state is, as for `apply` |

## Where the moved file goes

The first of these that is set decides:

1. `--out FILE` on the command line.
2. `spec.retarget.out` in the project's [`meridian.yaml`](../reference/modules.md#the-manifest),
   a pattern relative to the file moved:

   ```yaml
   spec:
     retarget:
       out: "moved/{name}.{target}.{ext}"
   ```

   `{name}` is the file's name without its extension, `{target}` the new target and
   `{ext}` the extension. It must hold `{target}`, so moves to two targets never write the
   same file, and it cannot be an absolute path.
3. The default, `{name}.{target}.{ext}`: `site.opentofu.yaml` beside `site.yaml`.

An existing file is never overwritten, and a missing directory is created. The JSON
result's `outFrom` says which one decided: `cli`, `manifest` or `default`. A global
setting, for every project of a user or an organisation, is
[issue 219](https://github.com/ZanattaMichael/meridian-core/issues/219).

## How it is tested

- A property test writes documents in many layouts (indent, CRLF, comments, key order,
  quoted target, no final newline) with identifiers full of YAML and Meridian syntax, and
  checks that every identifier reads back exactly on its own resource and that every
  other line is unchanged.
- The command is tested end to end against two copies of a stand-in tool: a full move, a
  partial one, several documents, moving back, each kind of new target and each refusal.
- In CI, a document applied with the real Terraform is moved to the real OpenTofu, whose
  first plan adopts everything and creates nothing; see
  [real-tool tests](../contributing/real-tool-tests.md#moving-from-terraform-to-opentofu).

## Limits

- YAML only. The file is rewritten as text, so comments and layout are kept;
  a JSON document is refused.
- An identifier holding `${` is written `$${`, the literal form the tools read.
- A resource that already has `adopt:` keeps it. A resource with
  `state: absent` gets none.
- A resource written in flow style (`- {id: x, ...}`) is listed as a note for
  you to add `adopt:` by hand.
- A resource the old tool manages but the file does not write, such as one a
  module makes, is listed as a note.
- `spec.targetVersion` and `spec.settings` are kept and listed as a note:
  they were written for the old tool.
- The old tool's state still holds every resource. After the new tool's
  apply, remove them from the old state with that tool's own command (for
  Terraform, `terraform state rm`, run in
  `.meridian/infrastructure/<name>/terraform`); never `meridian apply` the old file with
  resources removed, which would destroy them.

`--output json` prints one object; see [machine-readable output](../reference/cli-json.md#retarget).
