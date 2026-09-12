package puppet

import (
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// Emitter compiles a resource tree into a Puppet manifest and a node statement.
type Emitter struct{}

// New returns the Puppet emitter.
func New() *Emitter { return &Emitter{} }

// Name returns the target name documents select with spec.target.
func (e *Emitter) Name() string { return Name }

// SupportedResourceTypes lists every IR resource type this target maps.
func (e *Emitter) SupportedResourceTypes() []string { return SupportedResourceTypes() }

// Capabilities declares what Puppet can express natively.
//
// NativeNotify: the notify metaparameter is Puppet's own mechanism, so nothing
// is synthesised and SyntheticNotify stays false. That the mechanism is narrower
// than Ansible's is a matter for the mapping table, not for this declaration:
// notification either exists natively or it does not.
//
// RuntimeCondition is false, and it is the interesting half of this method. A
// catalog is compiled on the primary and shipped to the node, so by the time
// the resources are applied there is no expression left to evaluate. A document
// carrying runtimeWhen fails here with an explanation rather than compiling to
// a manifest that ignores it, which is the no-silent-capability-loss principle
// doing the only useful thing it can do.
func (e *Emitter) Capabilities() sdk.Capabilities {
	return sdk.Capabilities{
		NativeNotify:     true,
		SyntheticNotify:  false,
		RuntimeCondition: false,
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

	m, err := transform(g)
	if err != nil {
		return sdk.Artifact{}, nil, err
	}

	warnings, err := applyOrdering(g, m)
	if err != nil {
		return sdk.Artifact{}, nil, err
	}

	if err := validate(m); err != nil {
		return sdk.Artifact{}, nil, err
	}

	files, err := serialise(m)
	if err != nil {
		return sdk.Artifact{}, nil, err
	}
	return sdk.Artifact{Files: files}, warnings, nil
}

// The compile-time assertion is the contract check that matters most in this
// package: the plugin binary can only serve what satisfies sdk.Emitter.
var _ sdk.Emitter = (*Emitter)(nil)
