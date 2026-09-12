// Package graph builds the dependency graph over a document's resources and
// orders it deterministically.
//
// Two edge kinds are kept structurally distinct because they are not the same
// concept: dependsOn is a hard ordering constraint, while notifies is a
// conditional, deferred trigger that says nothing about order.
package graph

import (
	"fmt"
	"sort"
	"strings"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// EdgeKind distinguishes ordering constraints from deferred triggers.
type EdgeKind int

const (
	// HardOrder comes from dependsOn: From must be applied before To.
	HardOrder EdgeKind = iota
	// Notify comes from notifies: To is triggered if From reports a change.
	// It constrains nothing about apply order.
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

// Node is one resource in the graph.
type Node struct {
	ID string
	// Index is the resource's declaration order in the source document. It is
	// the only tiebreak used by the topological sort.
	Index    int
	Resource ir.Resource
}

// Edge is a directed relation between two resources. The same pair may carry
// both a HardOrder and a Notify edge; they are stored separately and never
// collapsed.
type Edge struct {
	From string
	To   string
	Kind EdgeKind
	// Action is the notify action (for example "restart"); empty for HardOrder.
	Action string
	// IfMissing is the dangling-edge policy declared on the source edge.
	IfMissing ir.IfMissing
	Pos       ir.Position
}

// Graph is a built, validated resource graph.
type Graph struct {
	nodes   []Node
	byID    map[string]int
	edges   []Edge
	outHard map[string][]int
	inHard  map[string][]int
}

// Error is a graph-construction or sort failure.
type Error struct {
	Msg   string
	Pos   ir.Position
	Cycle []string
}

func (e *Error) Error() string {
	var b strings.Builder
	if e.Pos.Line > 0 || e.Pos.File != "" {
		b.WriteString(e.Pos.String())
		b.WriteString(": ")
	}
	b.WriteString(e.Msg)
	if len(e.Cycle) > 0 {
		b.WriteString(": ")
		b.WriteString(strings.Join(e.Cycle, " -> "))
	}
	return b.String()
}

// BuildDocument builds the graph for one IR document.
func BuildDocument(doc ir.Document) (*Graph, error) {
	return Build(doc.ResourcesOf())
}

// Build constructs the graph from resources in declaration order.
//
// Edges pointing at resources that are not present are an error here: pruning a
// resource is the condition stage's job, and it applies each edge's ifMissing
// policy before the graph is built.
func Build(resources []ir.Resource) (*Graph, error) {
	g := &Graph{
		byID:    make(map[string]int, len(resources)),
		outHard: make(map[string][]int, len(resources)),
		inHard:  make(map[string][]int, len(resources)),
	}
	for i, r := range resources {
		if r.ID == "" {
			return nil, &Error{Msg: fmt.Sprintf("resource at declaration index %d has no id", i), Pos: r.Pos}
		}
		if _, dup := g.byID[r.ID]; dup {
			return nil, &Error{Msg: fmt.Sprintf("duplicate resource id %q", r.ID), Pos: r.Pos}
		}
		// Index comes from declaration order here rather than from the parsed
		// field, so a graph built from hand-made resources still sorts stably.
		g.byID[r.ID] = i
		g.nodes = append(g.nodes, Node{ID: r.ID, Index: i, Resource: r})
	}

	for _, r := range resources {
		for _, dep := range r.DependsOn {
			if dep.Resource == r.ID {
				return nil, &Error{Msg: fmt.Sprintf("resource %q depends on itself", r.ID), Pos: dep.Pos}
			}
			if _, ok := g.byID[dep.Resource]; !ok {
				return nil, &Error{
					Msg: fmt.Sprintf("resource %q depends on unknown resource %q", r.ID, dep.Resource),
					Pos: dep.Pos,
				}
			}
			g.addEdge(Edge{From: dep.Resource, To: r.ID, Kind: HardOrder, IfMissing: dep.IfMissing, Pos: dep.Pos})
		}
		for _, n := range r.Notifies {
			if n.Resource == r.ID {
				return nil, &Error{Msg: fmt.Sprintf("resource %q notifies itself", r.ID), Pos: n.Pos}
			}
			if _, ok := g.byID[n.Resource]; !ok {
				return nil, &Error{
					Msg: fmt.Sprintf("resource %q notifies unknown resource %q", r.ID, n.Resource),
					Pos: n.Pos,
				}
			}
			g.addEdge(Edge{From: r.ID, To: n.Resource, Kind: Notify, Action: n.Action, IfMissing: n.IfMissing, Pos: n.Pos})
		}
	}
	return g, nil
}

func (g *Graph) addEdge(e Edge) {
	idx := len(g.edges)
	g.edges = append(g.edges, e)
	if e.Kind == HardOrder {
		g.outHard[e.From] = append(g.outHard[e.From], idx)
		g.inHard[e.To] = append(g.inHard[e.To], idx)
	}
}

// Nodes returns the graph's nodes in declaration order.
func (g *Graph) Nodes() []Node { return append([]Node(nil), g.nodes...) }

// Edges returns every edge, in the order they were declared.
func (g *Graph) Edges() []Edge { return append([]Edge(nil), g.edges...) }

// EdgesOfKind returns only edges of the given kind, in declaration order.
func (g *Graph) EdgesOfKind(kind EdgeKind) []Edge {
	out := make([]Edge, 0, len(g.edges))
	for _, e := range g.edges {
		if e.Kind == kind {
			out = append(out, e)
		}
	}
	return out
}

// EdgesBetween returns every edge from one resource to another. A dependsOn and
// a notifies between the same pair both appear, distinctly.
func (g *Graph) EdgesBetween(from, to string) []Edge {
	out := make([]Edge, 0, 2)
	for _, e := range g.edges {
		if e.From == from && e.To == to {
			out = append(out, e)
		}
	}
	return out
}

// Node returns the node with the given id.
func (g *Graph) Node(id string) (Node, bool) {
	i, ok := g.byID[id]
	if !ok {
		return Node{}, false
	}
	return g.nodes[i], true
}

// Dependencies returns the ids a resource must be applied after, sorted by
// declaration order.
func (g *Graph) Dependencies(id string) []string {
	return g.neighbours(g.inHard[id], func(e Edge) string { return e.From })
}

// Dependents returns the ids that must be applied after a resource, sorted by
// declaration order.
func (g *Graph) Dependents(id string) []string {
	return g.neighbours(g.outHard[id], func(e Edge) string { return e.To })
}

func (g *Graph) neighbours(edgeIdx []int, pick func(Edge) string) []string {
	seen := make(map[string]struct{}, len(edgeIdx))
	out := make([]string, 0, len(edgeIdx))
	for _, i := range edgeIdx {
		id := pick(g.edges[i])
		if _, dup := seen[id]; dup {
			continue
		}
		seen[id] = struct{}{}
		out = append(out, id)
	}
	sort.SliceStable(out, func(a, b int) bool { return g.byID[out[a]] < g.byID[out[b]] })
	return out
}
