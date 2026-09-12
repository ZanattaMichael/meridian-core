package ansible

import (
	"fmt"
	"sort"
	"strings"

	"github.com/ZanattaMichael/meridian-core/internal/ast"
	"github.com/ZanattaMichael/meridian-core/internal/target"
)

// sortTasks establishes Ansible's ordering semantics: the dependency graph is
// flattened into one strict linear task sequence, because Ansible has no native
// way to express a graph at all.
//
// That flattening is lossy in one direction the operator should know about:
// resources that could have run concurrently are serialised, and nothing in the
// emitted playbook records that they were ever independent. Rather than hide
// that, the stage returns a warning naming the resources involved.
//
// It also re-checks the order it was handed, for a cycle first and then for a
// violated constraint. The graph stage should have made both impossible, but a
// target that trusts an invariant it never verifies fails silently when the
// invariant breaks.
func sortTasks(g *ast.ResourceGraph, tasks []task) ([]task, []target.Warning, error) {
	order := g.Order()
	pos := make(map[string]int, len(order))
	for i, id := range order {
		pos[id] = i
	}

	edges := g.EdgesOfKind(ast.HardOrder)
	indegree := make(map[string]int, len(order))
	dependents := make(map[string][]string, len(order))
	for _, id := range order {
		indegree[id] = 0
	}
	for _, e := range edges {
		// A hard-order edge points from the dependency to the resource that
		// declared it, so From must be applied before To.
		indegree[e.To]++
		dependents[e.From] = append(dependents[e.From], e.To)
	}

	warnings, placed := walkLevels(order, pos, indegree, dependents)
	if placed != len(order) {
		return nil, nil, &Error{
			Rule: "cycle",
			Msg: "a dependency cycle reached this target's sort stage; " +
				"the graph stage should have rejected it first",
		}
	}

	for _, e := range edges {
		if pos[e.From] > pos[e.To] {
			return nil, nil, &Error{
				Rule:     "ordering-violated",
				Resource: e.To,
				Pos:      e.Pos,
				Msg: fmt.Sprintf("resource %q is ordered before its dependency %q; "+
					"the graph stage produced an order this target cannot flatten", e.To, e.From),
			}
		}
	}

	// The tasks already arrive in the AST's topological order, which the checks
	// above just confirmed is a valid linear sequence. Returning them unchanged
	// keeps one ordering decision in the codebase instead of two.
	return tasks, warnings, nil
}

// walkLevels advances the graph one readiness level at a time. A level holding
// more than one resource is exactly the parallelism this target is about to
// discard, and the count of placed resources is how a cycle shows itself.
func walkLevels(order []string, pos map[string]int, indegree map[string]int, dependents map[string][]string) ([]target.Warning, int) {
	var warnings []target.Warning
	var ready []string
	for _, id := range order {
		if indegree[id] == 0 {
			ready = append(ready, id)
		}
	}

	placed := 0
	for len(ready) > 0 {
		sort.SliceStable(ready, func(i, j int) bool { return pos[ready[i]] < pos[ready[j]] })
		if len(ready) > 1 {
			warnings = append(warnings, target.Warning{
				Target: Name,
				Msg: fmt.Sprintf("resources %s are independent but will run in sequence; "+
					"this target flattens the dependency graph and cannot express parallelism",
					quoteList(ready)),
			})
		}
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
	return warnings, placed
}

func quoteList(ids []string) string {
	quoted := make([]string, 0, len(ids))
	for _, id := range ids {
		quoted = append(quoted, fmt.Sprintf("%q", id))
	}
	return strings.Join(quoted, ", ")
}
