package graph

import (
	"container/heap"
	"fmt"
)

// TopoSort returns the resource ids in an order that satisfies every dependsOn
// edge.
//
// Ties between independent resources are broken by declaration index, never by
// map iteration order, so the same graph always produces the same order. Notify
// edges are deliberately excluded: notifies is a deferred trigger, not an
// ordering constraint, and a notification loop is legal where a dependency loop
// is not.
func (g *Graph) TopoSort() ([]string, error) {
	indegree := make(map[string]int, len(g.nodes))
	for _, n := range g.nodes {
		indegree[n.ID] = 0
	}
	for _, e := range g.edges {
		if e.Kind != HardOrder {
			continue
		}
		indegree[e.To]++
	}

	ready := &indexHeap{}
	for _, n := range g.nodes {
		if indegree[n.ID] == 0 {
			heap.Push(ready, n.Index)
		}
	}

	order := make([]string, 0, len(g.nodes))
	for ready.Len() > 0 {
		idx := heap.Pop(ready).(int)
		id := g.nodes[idx].ID
		order = append(order, id)
		for _, edgeIdx := range g.outHard[id] {
			to := g.edges[edgeIdx].To
			indegree[to]--
			if indegree[to] == 0 {
				heap.Push(ready, g.byID[to])
			}
		}
	}

	if len(order) != len(g.nodes) {
		return nil, g.cycleError(indegree)
	}
	return order, nil
}

// TopoSortNodes is TopoSort returning full nodes rather than ids.
func (g *Graph) TopoSortNodes() ([]Node, error) {
	order, err := g.TopoSort()
	if err != nil {
		return nil, err
	}
	out := make([]Node, 0, len(order))
	for _, id := range order {
		out = append(out, g.nodes[g.byID[id]])
	}
	return out, nil
}

// cycleError finds one concrete cycle among the nodes the sort could not place
// and reports its full path, so the author sees which resources are involved
// rather than only that a cycle exists.
func (g *Graph) cycleError(indegree map[string]int) error {
	remaining := make(map[string]bool, len(indegree))
	for id, deg := range indegree {
		if deg > 0 {
			remaining[id] = true
		}
	}

	const (
		white = 0 // unvisited
		grey  = 1 // on the current search path
		black = 2 // fully explored
	)
	state := make(map[string]int, len(remaining))
	var path []string

	var walk func(id string) []string
	walk = func(id string) []string {
		state[id] = grey
		path = append(path, id)
		for _, edgeIdx := range g.outHard[id] {
			next := g.edges[edgeIdx].To
			if !remaining[next] {
				continue
			}
			switch state[next] {
			case grey:
				// Trim the path back to where the cycle opens, then close it.
				start := 0
				for i, n := range path {
					if n == next {
						start = i
						break
					}
				}
				return append(append([]string{}, path[start:]...), next)
			case white:
				if cycle := walk(next); cycle != nil {
					return cycle
				}
			}
		}
		path = path[:len(path)-1]
		state[id] = black
		return nil
	}

	// Walk in declaration order so the reported cycle is itself deterministic.
	for _, n := range g.nodes {
		if !remaining[n.ID] || state[n.ID] != white {
			continue
		}
		path = path[:0]
		if cycle := walk(n.ID); cycle != nil {
			return &Error{
				Msg:   fmt.Sprintf("dependency cycle among %d resources", len(cycle)-1),
				Pos:   g.nodes[g.byID[cycle[0]]].Resource.Pos,
				Cycle: cycle,
			}
		}
	}
	return &Error{Msg: "dependency cycle detected but could not be traced"}
}

// indexHeap is a min-heap of declaration indices. Using the index rather than
// the id keeps the tiebreak tied to source order instead of to string sorting.
type indexHeap []int

func (h indexHeap) Len() int           { return len(h) }
func (h indexHeap) Less(i, j int) bool { return h[i] < h[j] }
func (h indexHeap) Swap(i, j int)      { h[i], h[j] = h[j], h[i] }
func (h *indexHeap) Push(x any)        { *h = append(*h, x.(int)) }
func (h *indexHeap) Pop() any {
	old := *h
	n := len(old)
	v := old[n-1]
	*h = old[:n-1]
	return v
}
