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
| `--out FILE` | where to write. Default: `FILE` with the target before its extension, as in `site.opentofu.yaml`. An existing file is never overwritten |
| `--no-adopt` | read nothing from the old tool and write no `adopt:` lines |
| `--state-dir DIR` | where the old tool's state is, as for `apply` |

## Limits

- YAML only. The file is rewritten as text, so comments and layout are kept;
  a JSON document is refused.
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
