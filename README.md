# meridian-core
A unified configuration DSL that compiles to Ansible, Puppet, Chef, DSC, Terraform, and ARM — write once, target any configuration management or infrastructure-as-code backend.

## Status

Early implementation. The accepted specifications live in [`docs/`](docs/index.md) —
start with the [design plan](docs/plans/design-plan.md).

Milestones 1 to 3 of the [build sequence](docs/plans/design-plan.md#12-build-sequencing-milestones)
are in place, along with the first half of milestone 4:

- `internal/ir` — parsing and validation
- `internal/resolve` — hierarchical data resolution
- `internal/graph` — dependency graph and topological sort
- `internal/ast` — the target-agnostic resource tree every emitter consumes
- `pkg/sdk` — the public contract a target implements, and the server side of the plugin protocol
- `internal/plugin` — the host side: launching a plugin binary, the handshake, and the target registry
- `internal/fixtures` — the documents every target's tests compile, shared so their outputs can be read side by side
- `plugins/ansible` — the first target, built against the SDK alone and shipped as its own binary
- `plugins/puppet` — the second target, written against the SDK from its first line

A target is a separate process. The host launches the binary, reads one handshake line
saying where it is listening, checks the protocol version, and then talks to it over gRPC
as though it were a local emitter. Nothing above `internal/plugin` can tell a subprocess
target from a compiled-in one.

The two targets exist to prove the tree is target-agnostic. Both compile the same
documents from `internal/fixtures`, so their golden files differ only where the targets
themselves do. Ansible has no graph, so its sort stage flattens the tree into one task
list and warns about the parallelism that cost; Puppet's catalog is a graph, so the same
edges survive as `require` metaparameters and nothing is warned about. Where a target
cannot express what a document asks for, it refuses rather than emitting output that does
less: Puppet has no apply-time conditional and its refresh only restarts, so a document
carrying `runtimeWhen` or notifying a reload fails to compile with an explanation. Those
refusals are recorded as golden files too, because the wording an operator reads is
output like any other.

There is no CLI and no runner yet.

### Building and running the target plugins

```
go build -o meridian-target-ansible ./plugins/ansible/cmd/meridian-target-ansible
go build -o meridian-target-puppet  ./plugins/puppet/cmd/meridian-target-puppet
```

The binary is not meant to be run by hand: its stdout carries the protocol, and it exits
when the host closes its stdin. Drop it in a directory and a host discovers it by name.

### Regenerating the protocol

The generated protocol code under `pkg/sdk/pluginpb` is committed, so building this
repository never requires protoc. Regenerate it only when `proto/meridian/plugin/v1/plugin.proto`
changes, with protoc 29.3 and:

```
go install google.golang.org/protobuf/cmd/protoc-gen-go@v1.36.6
go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@v1.5.1
protoc --proto_path=proto \
  --go_out=. --go_opt=module=github.com/ZanattaMichael/meridian-core \
  --go-grpc_out=. --go-grpc_opt=module=github.com/ZanattaMichael/meridian-core \
  proto/meridian/plugin/v1/plugin.proto
```

### Tests

```
go build ./...
go test ./...          # includes the contract test, which builds the plugin binaries
go test -short ./...   # unit tests only
```
