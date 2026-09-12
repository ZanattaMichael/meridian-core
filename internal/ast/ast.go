// Package ast holds the target-agnostic resource tree that every emitter
// consumes.
//
// The AST is deliberately a different type from the IR rather than an alias for
// it. The IR is what an author wrote; the AST is what the compiler decided,
// after hierarchy resolution and condition pruning have run. By the time a tree
// reaches this package every `when` has already been evaluated and every
// surviving resource is unconditional, so an emitter never has to reason about
// whether a resource is really going to be applied.
package ast

import (
	"fmt"
	"sort"
	"strings"

	"github.com/ZanattaMichael/meridian-core/internal/graph"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// EdgeKind mirrors the graph package's distinction between a hard ordering
// constraint and a deferred trigger. It is redeclared here so an emitter can
// depend on the AST alone.
type EdgeKind int

const (
	// HardOrder comes from dependsOn: From must be applied before To.
	HardOrder EdgeKind = iota
	// Notify comes from notifies: To is triggered if From reports a change.
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
	// RuntimeWhen survives into the AST because it is not Meridian's to
	// evaluate: it is a condition checked on the node at the instant of apply,
	// and only targets whose Capabilities.RuntimeCondition is true may accept
	// it. Plain `when` never reaches here.
	RuntimeWhen string
	// Index is the resource's declaration order in the source document, kept so
	// a target that reorders resources still has the original tiebreak.
	Index int
	Pos   ir.Position
}

// Edge is a relation between two resources, carried into the AST with its kind
// intact so a target can compile dependsOn and notifies differently.
type Edge struct {
	From   string
	To     string
	Kind   EdgeKind
	Action string
	Pos    ir.Position
}

// ResourceGraph is a whole compiled document: its resources in a deterministic
// apply order, the edges between them, and the host they configure.
type ResourceGraph struct {
	// Name is the source document's metadata.name.
	Name string
	// Host is the resolved target host. It is a literal by this point, whether
	// it came from an inline declaration or an Infrastructure output.
	Host string
	// Target is the target name the document declared.
	Target string

	resources []Resource
	byID      map[string]int
	edges     []Edge
}

// Resources returns the resources in topological apply order.
func (g *ResourceGraph) Resources() []Resource { return append([]Resource(nil), g.resources...) }

// Edges returns every edge, ordered deterministically.
func (g *ResourceGraph) Edges() []Edge { return append([]Edge(nil), g.edges...) }

// EdgesOfKind returns only the edges of one kind, in the same relative order.
func (g *ResourceGraph) EdgesOfKind(kind EdgeKind) []Edge {
	var out []Edge
	for _, e := range g.edges {
		if e.Kind == kind {
			out = append(out, e)
		}
	}
	return out
}

// Resource looks up one resource by id.
func (g *ResourceGraph) Resource(id string) (Resource, bool) {
	i, ok := g.byID[id]
	if !ok {
		return Resource{}, false
	}
	return g.resources[i], true
}

// Order returns the apply order as a list of resource ids.
func (g *ResourceGraph) Order() []string {
	out := make([]string, 0, len(g.resources))
	for _, r := range g.resources {
		out = append(out, r.ID)
	}
	return out
}

// Canonical renders the whole tree in a stable textual form. It exists so
// determinism can be asserted on the AST itself, independent of any emitter.
func (g *ResourceGraph) Canonical() string {
	var b strings.Builder
	fmt.Fprintf(&b, "name=%s\nhost=%s\ntarget=%s\n", g.Name, g.Host, g.Target)
	for _, r := range g.resources {
		fmt.Fprintf(&b, "resource %s type=%s state=%s runtimeWhen=%s params=%s\n",
			r.ID, r.Type, r.State, r.RuntimeWhen, canonicalValue(r.Params))
	}
	for _, e := range g.edges {
		fmt.Fprintf(&b, "edge %s -> %s kind=%s action=%s\n", e.From, e.To, e.Kind, e.Action)
	}
	return b.String()
}

// Error is a failure to build an AST.
type Error struct {
	Msg string
	Pos ir.Position
	Err error
}

func (e *Error) Error() string {
	if e.Pos.Line > 0 || e.Pos.File != "" {
		return e.Pos.String() + ": " + e.Msg
	}
	return e.Msg
}

func (e *Error) Unwrap() error { return e.Err }

func errorf(pos ir.Position, format string, args ...any) *Error {
	return &Error{Pos: pos, Msg: fmt.Sprintf(format, args...)}
}

// Build turns a validated ResourceSet into a target-agnostic tree bound to the
// given host. The graph is built and topologically sorted here so every emitter
// sees the same deterministic order rather than re-deriving one.
func Build(rs *ir.ResourceSet, host string) (*ResourceGraph, error) {
	if rs == nil {
		return nil, &Error{Msg: "cannot build an AST from a nil ResourceSet"}
	}
	if host == "" {
		return nil, errorf(rs.Pos, "document %q has no resolved target host", rs.Metadata.Name)
	}
	for _, r := range rs.Spec.Resources {
		// A surviving `when` means the condition stage has not run. Emitting it
		// would either leak a conditional into supposedly static output or drop
		// it silently; both are worse than refusing.
		if r.When != "" {
			return nil, errorf(r.Pos,
				"resource %q still carries an unevaluated `when` expression; "+
					"the condition stage must prune before an AST is built", r.ID)
		}
	}

	g, err := graph.Build(rs.Spec.Resources)
	if err != nil {
		return nil, &Error{Msg: err.Error(), Err: err}
	}
	nodes, err := g.TopoSortNodes()
	if err != nil {
		return nil, &Error{Msg: err.Error(), Err: err}
	}

	out := &ResourceGraph{
		Name:   rs.Metadata.Name,
		Host:   host,
		Target: rs.Spec.Target,
		byID:   make(map[string]int, len(nodes)),
	}
	for _, n := range nodes {
		out.byID[n.ID] = len(out.resources)
		out.resources = append(out.resources, Resource{
			ID:          n.Resource.ID,
			Type:        n.Resource.Type,
			State:       n.Resource.State,
			Params:      copyValue(n.Resource.Params).(map[string]any),
			RuntimeWhen: n.Resource.RuntimeWhen,
			Index:       n.Index,
			Pos:         n.Resource.Pos,
		})
	}
	for _, e := range g.Edges() {
		out.edges = append(out.edges, Edge{
			From:   e.From,
			To:     e.To,
			Kind:   edgeKind(e.Kind),
			Action: e.Action,
			Pos:    e.Pos,
		})
	}
	sortEdges(out.edges)
	return out, nil
}

// New assembles a ResourceGraph from parts that are already decided.
//
// Build is the normal path: it derives the order from the graph. New exists for
// the cases where the order is the input rather than the output — an emitter
// test that must hand a stage an order it would never be given, and any future
// stage that rewrites a tree it has already built. The caller owns the order,
// so New checks nothing about it beyond resource identity.
func New(name, host, targetName string, resources []Resource, edges []Edge) *ResourceGraph {
	g := &ResourceGraph{
		Name:   name,
		Host:   host,
		Target: targetName,
		byID:   make(map[string]int, len(resources)),
	}
	for _, r := range resources {
		if _, dup := g.byID[r.ID]; dup {
			continue
		}
		g.byID[r.ID] = len(g.resources)
		r.Params = copyValue(r.Params).(map[string]any)
		g.resources = append(g.resources, r)
	}
	g.edges = append(g.edges, edges...)
	sortEdges(g.edges)
	return g
}

// BuildFromBundle resolves the document's targetHost against the bundle before
// building, so an Infrastructure output reaches the AST as a literal.
func BuildFromBundle(b *ir.Bundle, rs *ir.ResourceSet) (*ResourceGraph, error) {
	if b == nil {
		return nil, &Error{Msg: "cannot resolve a target host without a bundle"}
	}
	_, out, err := b.ResolveTargetHost(rs)
	if err != nil {
		return nil, &Error{Msg: err.Error(), Err: err}
	}
	return Build(rs, out.Value)
}

func edgeKind(k graph.EdgeKind) EdgeKind {
	if k == graph.Notify {
		return Notify
	}
	return HardOrder
}

// sortEdges orders edges by source apply position and then by their own fields,
// so two builds of the same document always list them identically.
func sortEdges(edges []Edge) {
	sort.SliceStable(edges, func(i, j int) bool {
		a, b := edges[i], edges[j]
		if a.From != b.From {
			return a.From < b.From
		}
		if a.Kind != b.Kind {
			return a.Kind < b.Kind
		}
		if a.To != b.To {
			return a.To < b.To
		}
		return a.Action < b.Action
	})
}
