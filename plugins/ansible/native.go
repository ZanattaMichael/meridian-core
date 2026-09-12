// Package ansible compiles a Meridian resource tree into an Ansible playbook
// and inventory.
//
// It is a target plugin: it depends on pkg/sdk and nothing else of Meridian's,
// which is the whole point of milestone 3. If this package can be written
// against the public SDK alone, so can a target Meridian's authors never
// anticipated. The binary that serves it over the plugin protocol is under
// cmd/; the package itself is an ordinary library, so the same emitter can be
// compiled in directly and tested without a process boundary.
//
// The four compile stages run in the order design plan §11.3 mandates:
// transform → sort → validate → emit. Each is a separate file, and emit does no
// mapping or ordering work of its own.
package ansible

import (
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
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
	Pos        sdk.Position
}

// playbook is a whole Ansible play.
type playbook struct {
	Name     string
	Hosts    string
	Tasks    []task
	Handlers []task
}
