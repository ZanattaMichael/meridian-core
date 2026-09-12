package sdk

import (
	"fmt"
	"sort"
	"strings"
)

// Position is a point inside a source document. Line and Column are 1-based; a
// zero Line means the position is unknown. It travels with every resource so a
// failure raised inside a plugin still points at the line the author wrote.
type Position struct {
	File   string
	DocIdx int
	Line   int
	Column int
}

func (p Position) String() string {
	loc := p.File
	if loc == "" {
		loc = "<input>"
	}
	if p.DocIdx > 0 {
		loc = fmt.Sprintf("%s[doc %d]", loc, p.DocIdx)
	}
	if p.Line > 0 {
		loc = fmt.Sprintf("%s:%d", loc, p.Line)
		if p.Column > 0 {
			loc = fmt.Sprintf("%s:%d", loc, p.Column)
		}
	}
	return loc
}

// Known reports whether the position identifies anything at all.
func (p Position) Known() bool { return p.File != "" || p.Line > 0 }

// EdgeKind distinguishes an ordering constraint from a deferred trigger. The
// two are kept structurally distinct because targets compile them differently:
// one becomes task order, the other becomes a handler.
type EdgeKind int

const (
	// HardOrder comes from dependsOn: From must be applied before To.
	HardOrder EdgeKind = iota
	// Notify comes from notifies: To is triggered if From reports a change. It
	// constrains nothing about apply order.
	Notify
)

func (k EdgeKind) String() string {
	switch k {
	case HardOrder:
		return "dependsOn"
	case Notify:
		return "notifies"
	}
	return fmt.Sprintf("EdgeKind(%d)", int(k))
}

// Resource is one unconditional unit of desired state.
type Resource struct {
	ID     string
	Type   string
	State  string
	Params map[string]any
	// RuntimeWhen is a condition checked on the node at the instant of apply.
	// Only a target whose Capabilities.RuntimeCondition is true may accept one;
	// plain `when` is evaluated and pruned long before a plugin sees the tree.
	RuntimeWhen string
	// Index is the resource's declaration order in the source document, kept so
	// a target that reorders resources still has the original tiebreak.
	Index int
	Pos   Position
}

// Edge is a relation between two resources, carried with its kind intact.
type Edge struct {
	From   string
	To     string
	Kind   EdgeKind
	Action string
	Pos    Position
}

// ResourceGraph is a whole compiled document.
//
// Resources are already in a deterministic apply order that satisfies every
// ordering edge. A target is free to reorder them under its own semantics, but
// it never has to derive an order from scratch, and two hosts compiling the
// same document hand a plugin the same sequence.
type ResourceGraph struct {
	// Name is the source document's metadata.name.
	Name string
	// Host is the resolved target host, a literal by this point whether it was
	// written inline or read from an Infrastructure output.
	Host string
	// Target is the target name the document selected.
	Target string

	Resources []Resource
	Edges     []Edge
}

// EdgesOfKind returns only the edges of one kind, in their existing order.
func (g *ResourceGraph) EdgesOfKind(kind EdgeKind) []Edge {
	var out []Edge
	for _, e := range g.Edges {
		if e.Kind == kind {
			out = append(out, e)
		}
	}
	return out
}

// Resource looks up one resource by id.
func (g *ResourceGraph) Resource(id string) (Resource, bool) {
	for _, r := range g.Resources {
		if r.ID == id {
			return r, true
		}
	}
	return Resource{}, false
}

// Order returns the apply order as a list of resource ids.
func (g *ResourceGraph) Order() []string {
	out := make([]string, 0, len(g.Resources))
	for _, r := range g.Resources {
		out = append(out, r.ID)
	}
	return out
}

// Canonical renders the tree in a stable textual form, so a test can assert
// that what left the host is exactly what reached the plugin.
func (g *ResourceGraph) Canonical() string {
	var b strings.Builder
	fmt.Fprintf(&b, "name=%s\nhost=%s\ntarget=%s\n", g.Name, g.Host, g.Target)
	for _, r := range g.Resources {
		fmt.Fprintf(&b, "resource %s type=%s state=%s runtimeWhen=%s index=%d params=%s\n",
			r.ID, r.Type, r.State, r.RuntimeWhen, r.Index, CanonicalValue(r.Params))
	}
	for _, e := range g.Edges {
		fmt.Fprintf(&b, "edge %s -> %s kind=%s action=%s\n", e.From, e.To, e.Kind, e.Action)
	}
	return b.String()
}

// Data is resolved hierarchy data handed to a plugin alongside the tree.
type Data map[string]any

// Capabilities is a target's honest declaration of what it can express. It is
// the mechanism behind the no-silent-capability-loss principle: a document that
// needs something the target lacks fails at compile time and says so, rather
// than compiling to output that quietly does less.
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
// It is data, not files on disk, so a plan stays a pure function.
type Artifact struct {
	Files map[string]string
}

// Paths returns the artifact's file paths sorted, so a caller that prints or
// hashes an artifact never depends on map iteration order.
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
