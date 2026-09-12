package ansible

import (
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
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
func (e *Emitter) Capabilities() sdk.Capabilities {
	return sdk.Capabilities{
		NativeNotify:     true,
		SyntheticNotify:  false,
		RuntimeCondition: true,
	}
}

// Emit runs the target's four compile stages in the order the design plan
// mandates: transform, then sort, then validate, then serialise.
//
// The resolved hierarchy data is not consumed here because Meridian's default
// is to bake resolved values into the IR as literals before a tree is built;
// the parameter is part of the emitter contract and will matter to the opt-in
// native-passthrough mode, which no target implements yet.
func (e *Emitter) Emit(g *sdk.ResourceGraph, _ sdk.Data) (sdk.Artifact, []sdk.Warning, error) {
	if g == nil {
		return sdk.Artifact{}, nil, &Error{Target: Name, Rule: "no-input", Msg: "cannot emit a nil resource graph"}
	}

	pb, err := transform(g)
	if err != nil {
		return sdk.Artifact{}, nil, err
	}

	tasks, warnings, err := sortTasks(g, pb.Tasks)
	if err != nil {
		return sdk.Artifact{}, nil, err
	}
	pb.Tasks = tasks

	if err := validate(pb); err != nil {
		return sdk.Artifact{}, nil, err
	}

	files, err := serialise(pb)
	if err != nil {
		return sdk.Artifact{}, nil, err
	}
	return sdk.Artifact{Files: files}, warnings, nil
}

// The compile-time assertion is the contract check that matters most in this
// package: the plugin binary can only serve what satisfies sdk.Emitter.
var _ sdk.Emitter = (*Emitter)(nil)
