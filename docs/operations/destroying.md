# Destroying what was applied

`meridian destroy` removes what each document's tool made, last stage first:

```
meridian plan --destroy site.yaml     # preview; changes nothing
meridian destroy site.yaml            # asks, then removes
meridian destroy --yes site.yaml      # no question, for a pipeline
```

Each stage's tool runs its own destroy, so what goes is what the tool's state says it made.
A destroyed stage is removed from the [run-state](../reference/state.md): `drift` then reports
it as never applied, and a later `apply` creates it again.

## Order

Stages run in the reverse of the apply order: ResourceSets before the Infrastructure they run
on, and within each kind, last written first. A ResourceSet whose host comes from an
Infrastructure output still finds that output, because the Infrastructure is not removed yet.

## Which targets destroy

| Target | Destroy |
|---|---|
| `terraform`, `opentofu` | `plan -destroy -out=…`, then `apply` of that plan |
| `pulumi` | `stack export` for the preview, then `pulumi destroy`; the stack is kept |
| `cloudformation` | `list-stack-resources` for the preview, then `delete-stack` |
| `kubernetes` | `kubectl delete --wait` of the manifests' objects |
| `helm` | `helm uninstall --wait`; the namespace is kept |
| `docker` | `docker compose down --volumes --remove-orphans`; images are kept |
| `podman` | stops and removes the units it installed; volumes, images and secrets are kept |
| `bicep`, `arm` | each resource in the deployment record's `outputResources` deleted, children first, then the record |
| `ansible` | the document's inverse, compiled into a play at destroy time and applied; `--check --diff` for the preview (part 1 of issue 270) |
| `chef`, `puppet`, `openvox`, `salt`, `dsc`, `cloud-init`, `packer` | none: the stage is `kept` |

Chef, Puppet, OpenVox, Salt, DSC, cloud-init and Packer keep no record of what they made, so
they cannot destroy. Destroy the host or image they configured, with the Infrastructure
document that made it. Ansible is the exception: a play names every resource it changes, so
Meridian compiles the document's inverse and applies it (see [the Ansible target](../targets/ansible.md#destroy)).
A resource with no absent state — an `exec`, or a native module with none — is left in place
and named in the output with the rule `unsupported-destroy`.

## What a stage can end as

| Status | Meaning |
|---|---|
| `destroyed` | the tool removed everything; the stage is forgotten |
| `kept` (`cannot-destroy`) | the target has no destroy |
| `kept` (`never-applied`) | the run-state has no apply of the stage |
| `failed` | the tool failed; the changes it reported before failing are listed, the stage stays recorded as applied, and the run stops |
| `not-run` | an earlier stage failed |

Run `destroy` again after a failure: stages already destroyed are `kept` (`never-applied`).

## Safety

- It asks `Type yes to destroy:` on a terminal. Without one, or with `--output json`, it is
  refused unless `--yes` is given.
- It takes the [state lock](../concepts/run-safety.md#one-run-at-a-time) and is recorded with
  mode `destroy`. Each removal is a `resource.changed` event with action `delete`; a kept
  stage is `step.skipped` with reason `cannot-destroy` or `never-applied`.
- Ctrl-C and `--stage-timeout` stop it as they stop an apply.
- `--policy`, `--resume` and `--plan` are refused: a policy is not asked about a destroy yet.

## For plugin authors

A runner declares `destroys() -> true` and implements `plan_destroy` and `destroy`
([writing a plugin](../contributing/writing-a-plugin.md)). Protocol: `Runner.Destroy`,
`PlanRequest.destroy`, `DescribeResponse.destroys`. A plugin built before them is `kept`.
