package puppet

import (
	"fmt"
	"sort"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// applyOrdering establishes Puppet's ordering semantics: every dependency edge
// becomes a require metaparameter on the dependent resource.
//
// This is where Puppet and Ansible diverge most visibly. Ansible has no way to
// express a graph, so its emitter flattens the tree into one linear task list
// and warns that resources which could have run concurrently will not. Puppet's
// catalog *is* a graph, so nothing is flattened and there is nothing to warn
// about: the edges the author wrote survive into the output one for one, and
// the agent is free to apply independent resources in whatever order it likes.
//
// The stage still re-checks the order it was handed, for a cycle first and then
// for a violated constraint. Puppet would catch a cycle itself when it compiles
// the catalog, but it would do so on the operator's primary with a message
// about resource titles rather than about the document they wrote.
func applyOrdering(g *sdk.ResourceGraph, m *manifest) ([]sdk.Warning, error) {
	index := make(map[string]int, len(m.Resources))
	for i, r := range m.Resources {
		index[r.ResourceID] = i
	}

	order := g.Order()
	pos := make(map[string]int, len(order))
	for i, id := range order {
		pos[id] = i
	}

	edges := g.EdgesOfKind(sdk.HardOrder)
	if err := checkAcyclic(order, edges); err != nil {
		return nil, err
	}

	// A notify metaparameter already orders the notifier before the resource it
	// notifies, so a require that says the same thing is noise in the output
	// without being wrong. Collecting what notify covers first keeps the two
	// from both appearing on the same pair.
	implied := make(map[string]bool)
	for _, r := range m.Resources {
		for _, ref := range r.Notify {
			implied[r.ResourceID+"\x00"+ref.Title] = true
		}
	}

	seen := make(map[string]bool)
	for _, e := range edges {
		// A hard-order edge points from the dependency to the resource that
		// declared it, so From must be applied before To, and it is To that
		// carries the require.
		if pos[e.From] > pos[e.To] {
			return nil, &Error{
				Target:   Name,
				Rule:     "ordering-violated",
				Resource: e.To,
				Pos:      e.Pos,
				Msg: fmt.Sprintf("resource %q is ordered before its dependency %q; "+
					"the graph stage produced an order this target cannot trust", e.To, e.From),
			}
		}
		if implied[e.From+"\x00"+e.To] {
			continue
		}
		i, ok := index[e.To]
		if !ok {
			return nil, &Error{
				Target:   Name,
				Rule:     "unknown-dependency",
				Resource: e.To,
				Pos:      e.Pos,
				Msg:      fmt.Sprintf("dependency edge names resource %q, which this manifest does not declare", e.To),
			}
		}
		ref := reference{Type: m.Resources[index[e.From]].Type, Title: e.From}
		key := e.To + "\x00" + ref.String()
		if seen[key] {
			continue
		}
		seen[key] = true
		m.Resources[i].Require = append(m.Resources[i].Require, ref)
	}

	for i := range m.Resources {
		refs := m.Resources[i].Require
		sort.Slice(refs, func(a, b int) bool { return refs[a].String() < refs[b].String() })
	}
	return nil, nil
}

// checkAcyclic walks the graph one readiness level at a time. A resource that
// is never placed is one a cycle is holding back.
//
// The graph stage should have made this impossible. A target that trusts an
// invariant it never verifies fails silently when the invariant breaks, and
// here the failure would be a catalog Puppet rejects on the operator's primary
// rather than a message about the document they wrote.
func checkAcyclic(order []string, edges []sdk.Edge) error {
	indegree := make(map[string]int, len(order))
	dependents := make(map[string][]string, len(order))
	for _, id := range order {
		indegree[id] = 0
	}
	for _, e := range edges {
		indegree[e.To]++
		dependents[e.From] = append(dependents[e.From], e.To)
	}

	var ready []string
	for _, id := range order {
		if indegree[id] == 0 {
			ready = append(ready, id)
		}
	}

	placed := 0
	for len(ready) > 0 {
		var next []string
		for _, id := range ready {
			placed++
			for _, dep := range dependents[id] {
				indegree[dep]--
				if indegree[dep] == 0 {
					next = append(next, dep)
				}
			}
		}
		ready = next
	}
	if placed != len(order) {
		return &Error{
			Target: Name,
			Rule:   "cycle",
			Msg: "a dependency cycle reached this target's sort stage; " +
				"the graph stage should have rejected it first",
		}
	}
	return nil
}
