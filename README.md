# meridian-core
A unified configuration DSL that compiles to Ansible, Puppet, Chef, DSC, Terraform, and ARM — write once, target any configuration management or infrastructure-as-code backend.

## Status

Early implementation. The accepted specifications live in [`docs/`](docs/index.md) —
start with the [design plan](docs/plans/design-plan.md).

Milestone 1 of the [build sequence](docs/plans/design-plan.md#12-build-sequencing-milestones)
is in place: `internal/ir` (parsing and validation), `internal/resolve` (hierarchical
data resolution) and `internal/graph` (dependency graph and topological sort). There
are no emitters yet, and no CLI.

```
go build ./...
go test ./...
```
