// Package target holds the shapes every emitter implements.
//
// These are the interfaces design plan §11.4 specifies for the public plugin
// SDK. They live in internal/ for now on purpose: milestone 2 builds exactly one
// emitter with no plugin boundary, and promoting a boundary to a public package
// before a second implementation has pushed on it would freeze guesses as API.
// Milestone 3 extracts this package into pkg/sdk against an already-correct
// emitter.
package target

import "sort"

// Capabilities is a target's honest declaration of what it can express. It is
// the mechanism behind the no-silent-capability-loss principle: a document that
// needs something the target lacks fails at compile time and says so, rather
// than emitting output that quietly does less.
type Capabilities struct {
	// NativeNotify means the target has its own change-triggered mechanism.
	NativeNotify bool
	// SyntheticNotify means notification is emulated, at a cost the plan must
	// surface to the operator.
	SyntheticNotify bool
	// RuntimeCondition means the target can evaluate a condition at the instant
	// of apply, which is what runtimeWhen requires.
	RuntimeCondition bool
}

// Artifact is the complete output of a compile: relative path to file content.
// It is data, not files on disk, so `plan` stays a pure function.
type Artifact struct {
	Files map[string]string
}

// Paths returns the artifact's file paths in sorted order, so callers that
// print or hash an artifact never depend on map iteration.
func (a Artifact) Paths() []string {
	out := make([]string, 0, len(a.Files))
	for p := range a.Files {
		out = append(out, p)
	}
	sort.Strings(out)
	return out
}

// Warning is a compile-time notice that does not fail the build but changes
// what an operator should expect from the emitted artifact.
type Warning struct {
	// Target is the emitter that raised it.
	Target string
	// Resource is the offending resource id, when the warning is about one.
	Resource string
	Msg      string
}

func (w Warning) String() string {
	if w.Resource != "" {
		return w.Target + ": " + w.Resource + ": " + w.Msg
	}
	return w.Target + ": " + w.Msg
}
