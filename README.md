# meridian-core
A unified configuration DSL that compiles to Ansible, Puppet, Chef, DSC, Terraform, and ARM — write once, target any configuration management or infrastructure-as-code backend.

## Status

Early implementation. The accepted specifications live in [`docs/`](docs/index.md) —
start with the [design plan](docs/plans/design-plan.md).

Milestones 1 to 3 of the [build sequence](docs/plans/design-plan.md#12-build-sequencing-milestones)
are in place:

- `internal/ir` — parsing and validation
- `internal/resolve` — hierarchical data resolution
- `internal/graph` — dependency graph and topological sort
- `internal/ast` — the target-agnostic resource tree every emitter consumes
- `pkg/sdk` — the public contract a target implements, and the server side of the plugin protocol
- `internal/plugin` — the host side: launching a plugin binary, the handshake, and the target registry
- `plugins/ansible` — the first target, built against the SDK alone and shipped as its own binary

A target is a separate process. The host launches the binary, reads one handshake line
saying where it is listening, checks the protocol version, and then talks to it over gRPC
as though it were a local emitter. Nothing above `internal/plugin` can tell a subprocess
target from a compiled-in one.

There is no CLI and no runner yet.

### Building and running the Ansible plugin

```
go build -o meridian-target-ansible ./plugins/ansible/cmd/meridian-target-ansible
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
go test ./...          # includes the contract test, which builds the plugin binary
go test -short ./...   # unit tests only
```
