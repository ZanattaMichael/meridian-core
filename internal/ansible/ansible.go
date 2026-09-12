package ansible

import (
	"github.com/ZanattaMichael/meridian-core/internal/ast"
	"github.com/ZanattaMichael/meridian-core/internal/resolve"
	"github.com/ZanattaMichael/meridian-core/internal/target"
)

// Emitter compiles a resource tree into an Ansible playbook and inventory.
type Emitter struct{}

// New returns the Ansible emitter.
func New() *Emitter { return &Emitter{} }

// Name returns the target name documents select with spec.target.
func (e *Emitter) Name() string { return Name }

// SupportedResourceTypes lists every IR resource type this target maps.
func (e *Emitter) SupportedResourceTypes() []string { return SupportedResourceTypes() }

// Capabilities declares what Ansible can express natively.
//
// NativeNotify: notify/handlers is Ansible's own mechanism, so nothing is
// synthesised and SyntheticNotify stays false.
//
// RuntimeCondition: a task's `when:` is evaluated on the node at apply time,
// which is what runtimeWhen needs. That capability is deliberately not used for
// ordinary `when`, which Meridian evaluates and prunes long before this stage.
func (e *Emitter) Capabilities() target.Capabilities {
	return target.Capabilities{
		NativeNotify:     true,
		SyntheticNotify:  false,
		RuntimeCondition: true,
	}
}

// Emit runs the target's four compile stages in the order the design plan
// mandates: transform, then sort, then validate, then serialise.
//
// The resolved hierarchy data is not consumed here because Meridian's default
// is to bake resolved values into the IR as literals before an AST is built;
// the parameter is part of the emitter contract and will matter to the opt-in
// native-passthrough mode, which no target implements yet.
func (e *Emitter) Emit(g *ast.ResourceGraph, _ resolve.Data) (target.Artifact, []target.Warning, error) {
	if g == nil {
		return target.Artifact{}, nil, &Error{Rule: "no-input", Msg: "cannot emit a nil resource graph"}
	}

	pb, err := transform(g)
	if err != nil {
		return target.Artifact{}, nil, err
	}

	tasks, warnings, err := sortTasks(g, pb.Tasks)
	if err != nil {
		return target.Artifact{}, nil, err
	}
	pb.Tasks = tasks

	if err := validate(pb); err != nil {
		return target.Artifact{}, nil, err
	}

	files, err := serialise(pb)
	if err != nil {
		return target.Artifact{}, nil, err
	}
	return target.Artifact{Files: files}, warnings, nil
}

var _ target.Emitter = (*Emitter)(nil)
