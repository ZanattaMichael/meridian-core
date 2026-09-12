// Package ansible is the Ansible emitter, compiled directly into the binary.
//
// It is deliberately not a plugin yet. Milestone 2 of the design plan builds one
// emitter with no plugin boundary so the AST shape is validated end-to-end
// before a gRPC protocol freezes it; milestone 3 extracts this package across
// the public SDK unchanged.
//
// The four compile stages run in the order design plan §11.3 mandates:
// transform → sort → validate → emit. Each is a separate file, and emit does no
// mapping or ordering work of its own.
package ansible

import (
	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// Name is the target name a document selects with spec.target.
const Name = "ansible"

// task is one native Ansible task or handler: the module to run, its arguments,
// and the structural keys around it. It is the native AST this target's
// transform stage produces and its emit stage serialises.
type task struct {
	// Name is the task's `name:` key, which is also the identifier handlers are
	// notified by, so it must be unique within the play.
	Name string
	// Module is the fully-qualified module name, e.g. ansible.builtin.package.
	Module string
	// Args are the module's arguments, keyed by native argument name.
	Args map[string]any
	// Notify lists handler names this task triggers on change.
	Notify []string
	// When is a target-native runtime conditional. It is only ever populated
	// from a resource's runtimeWhen: a compile-time `when` is pruned long
	// before this stage and must never reach the output.
	When string

	// ResourceID and Pos are provenance, kept so a validation failure can name
	// the resource the author wrote rather than the task it became.
	ResourceID string
	Pos        ir.Position
}

// playbook is a whole Ansible play.
type playbook struct {
	Name     string
	Hosts    string
	Tasks    []task
	Handlers []task
}
