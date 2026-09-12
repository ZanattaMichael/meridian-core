package sdk

// Emitter compiles a target-agnostic resource tree into one target's native
// syntax.
//
// It is deliberately separate from a runner: Emit has no side effects, which is
// what lets `meridian plan` be a pure function. A plugin that needs to shell out
// to a real CLI does so behind a different interface, outside the compile chain.
type Emitter interface {
	// Name is the target name documents select with spec.target.
	Name() string
	// SupportedResourceTypes lists every IR resource type this target maps,
	// sorted. Anything outside it is a compile error, never a best-effort guess.
	SupportedResourceTypes() []string
	// Capabilities declares what this target can express natively.
	Capabilities() Capabilities
	// Emit compiles the tree. Warnings describe what the target could not
	// preserve; an error means it could not compile at all, and should be a
	// *CompileError so the host can report it against the author's own source.
	Emit(g *ResourceGraph, data Data) (Artifact, []Warning, error)
}
