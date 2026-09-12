package sdk

import (
	"errors"
	"fmt"
	"strings"
)

// CompileError is a target refusing to compile something.
//
// It carries the resource, the rule and the source position separately rather
// than only a message because those three survive the process boundary as
// fields: a host can report a plugin's refusal against the author's own line,
// which a flattened error string cannot do.
//
// A CompileError is not a fault. A target rejecting a resource type it cannot
// express is the capability contract working as designed, which is why it
// travels as part of a successful response and never as a transport error.
type CompileError struct {
	// Target is the emitter that refused; filled in by the host when the error
	// crosses the boundary, so a message never leaves the reader guessing which
	// target spoke.
	Target string
	// Resource is the offending resource id.
	Resource string
	// Rule is a stable, machine-readable name for what was violated, for
	// example "unsupported-resource-type".
	Rule string
	Msg  string
	Pos  Position
}

func (e *CompileError) Error() string {
	var b strings.Builder
	if e.Pos.Known() {
		b.WriteString(e.Pos.String())
		b.WriteString(": ")
	}
	if e.Target != "" {
		b.WriteString(e.Target)
	}
	if e.Resource != "" {
		fmt.Fprintf(&b, ": resource %q", e.Resource)
	}
	if e.Rule != "" {
		fmt.Fprintf(&b, " [%s]", e.Rule)
	}
	b.WriteString(": ")
	b.WriteString(e.Msg)
	return b.String()
}

// Errorf builds a CompileError against one resource.
func Errorf(r Resource, rule, format string, args ...any) *CompileError {
	return &CompileError{Resource: r.ID, Rule: rule, Msg: fmt.Sprintf(format, args...), Pos: r.Pos}
}

// as is errors.As, wrapped so wire.go does not import errors for one call and
// so the unwrapping rule lives next to the error it unwraps.
func as(err error, target **CompileError) bool {
	return errors.As(err, target)
}
