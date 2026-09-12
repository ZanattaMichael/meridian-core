# meridian-core
A unified configuration DSL that compiles to Ansible, Puppet, Chef, DSC, Terraform, and ARM — write once, target any configuration management or infrastructure-as-code backend.

## Status

Early implementation. The accepted specifications live in [`docs/`](docs/index.md) —
start with the [design plan](docs/plans/design-plan.md).

Milestones 1 and 2 of the [build sequence](docs/plans/design-plan.md#12-build-sequencing-milestones)
are in place:

- `internal/ir` — parsing and validation
- `internal/resolve` — hierarchical data resolution
- `internal/graph` — dependency graph and topological sort
- `internal/ast` — the target-agnostic resource tree every emitter consumes
- `internal/target` — the emitter contract, pre-SDK
- `internal/ansible` — one emitter, compiled in directly rather than loaded as a plugin

Ansible is deliberately hardcoded for now: the point of milestone 2 is to validate the
AST shape end-to-end before a plugin protocol freezes it. There is no CLI, no plugin
boundary and no runner yet.

```
go build ./...
go test ./...
```
