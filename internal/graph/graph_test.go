package graph

import (
	"fmt"
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// res builds a resource with dependsOn edges, in the shape the parser produces.
func res(id string, dependsOn ...string) ir.Resource {
	r := ir.Resource{ID: id, Type: "package"}
	for _, d := range dependsOn {
		r.DependsOn = append(r.DependsOn, ir.Dependency{Resource: d})
	}
	return r
}

func mustBuild(t *testing.T, resources ...ir.Resource) *Graph {
	t.Helper()
	g, err := Build(resources)
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	return g
}

func mustSort(t *testing.T, g *Graph) []string {
	t.Helper()
	order, err := g.TopoSort()
	if err != nil {
		t.Fatalf("TopoSort: %v", err)
	}
	return order
}

func TestTopoSortRespectsDependencies(t *testing.T) {
	g := mustBuild(t,
		res("nginx_conf", "nginx_pkg"),
		res("nginx_svc", "nginx_pkg", "nginx_conf"),
		res("nginx_pkg"),
	)
	order := mustSort(t, g)
	pos := map[string]int{}
	for i, id := range order {
		pos[id] = i
	}
	if pos["nginx_pkg"] > pos["nginx_conf"] || pos["nginx_conf"] > pos["nginx_svc"] {
		t.Fatalf("order = %v, want pkg before conf before svc", order)
	}
}

func TestTopoSortTiebreaksByDeclarationIndex(t *testing.T) {
	// Ten resources with no edges at all: only the declaration index can
	// decide their order. Map iteration would scramble this every run.
	var resources []ir.Resource
	var want []string
	for i := 0; i < 10; i++ {
		id := fmt.Sprintf("r%02d", 9-i) // ids deliberately counter to declaration order
		resources = append(resources, res(id))
		want = append(want, id)
	}
	g := mustBuild(t, resources...)
	for run := 0; run < 50; run++ {
		order := mustSort(t, g)
		if len(order) != len(want) {
			t.Fatalf("run %d: got %d ids, want %d", run, len(order), len(want))
		}
		for i := range want {
			if order[i] != want[i] {
				t.Fatalf("run %d: order = %v, want %v (declaration order, not id order)", run, order, want)
			}
		}
	}
}

func TestTopoSortIsDeterministicAcrossRebuilds(t *testing.T) {
	build := func() *Graph {
		return mustBuild(t,
			res("a"),
			res("b", "a"),
			res("c", "a"),
			res("d", "b", "c"),
			res("e"),
			res("f", "e"),
		)
	}
	want := strings.Join(mustSort(t, build()), ",")
	for i := 0; i < 50; i++ {
		if got := strings.Join(mustSort(t, build()), ","); got != want {
			t.Fatalf("run %d: order = %s, want %s", i, got, want)
		}
	}
}

func TestTopoSortNodesMatchesTopoSort(t *testing.T) {
	g := mustBuild(t, res("a"), res("b", "a"))
	nodes, err := g.TopoSortNodes()
	if err != nil {
		t.Fatalf("TopoSortNodes: %v", err)
	}
	order := mustSort(t, g)
	if len(nodes) != len(order) {
		t.Fatalf("got %d nodes, want %d", len(nodes), len(order))
	}
	for i := range order {
		if nodes[i].ID != order[i] {
			t.Errorf("nodes[%d].ID = %q, want %q", i, nodes[i].ID, order[i])
		}
	}
}

func TestCycleErrorReportsTheFullPath(t *testing.T) {
	g := mustBuild(t,
		res("a", "c"),
		res("b", "a"),
		res("c", "b"),
		res("unrelated"),
	)
	_, err := g.TopoSort()
	if err == nil {
		t.Fatal("want a cycle error")
	}
	ge, ok := err.(*Error)
	if !ok {
		t.Fatalf("got %T, want *Error: %v", err, err)
	}
	if len(ge.Cycle) != 4 {
		t.Fatalf("cycle = %v, want four entries closing the loop", ge.Cycle)
	}
	if ge.Cycle[0] != ge.Cycle[len(ge.Cycle)-1] {
		t.Errorf("cycle = %v, want it to close on the resource it opened with", ge.Cycle)
	}
	for _, id := range []string{"a", "b", "c"} {
		if !strings.Contains(err.Error(), id) {
			t.Errorf("error = %v, want it to name %q", err, id)
		}
	}
	if strings.Contains(err.Error(), "unrelated") {
		t.Errorf("error = %v, want it to exclude resources outside the cycle", err)
	}
}

func TestCycleErrorIsDeterministic(t *testing.T) {
	build := func() *Graph {
		return mustBuild(t, res("a", "c"), res("b", "a"), res("c", "b"), res("x", "y"), res("y", "x"))
	}
	_, err := build().TopoSort()
	if err == nil {
		t.Fatal("want a cycle error")
	}
	want := err.Error()
	for i := 0; i < 50; i++ {
		_, err := build().TopoSort()
		if err == nil {
			t.Fatalf("run %d: want a cycle error", i)
		}
		if got := err.Error(); got != want {
			t.Fatalf("run %d: error = %q, want %q", i, got, want)
		}
	}
}

func TestTwoNodeCycleIsDetected(t *testing.T) {
	g := mustBuild(t, res("a", "b"), res("b", "a"))
	_, err := g.TopoSort()
	if err == nil || !strings.Contains(err.Error(), "cycle") {
		t.Fatalf("error = %v, want a cycle error", err)
	}
}

func TestSelfDependencyIsRejectedAtBuildTime(t *testing.T) {
	_, err := Build([]ir.Resource{res("a", "a")})
	if err == nil {
		t.Fatal("want an error at build time, not a cycle error at sort time")
	}
	if !strings.Contains(err.Error(), `resource "a" depends on itself`) {
		t.Errorf("error = %v", err)
	}
}

func TestSelfNotificationIsRejectedAtBuildTime(t *testing.T) {
	r := ir.Resource{ID: "a", Type: "service", Notifies: []ir.Notification{{Resource: "a", Action: "restart"}}}
	_, err := Build([]ir.Resource{r})
	if err == nil || !strings.Contains(err.Error(), `resource "a" notifies itself`) {
		t.Fatalf("error = %v, want a self-notification rejection", err)
	}
}

func TestBuildRejectsUnknownAndDuplicateIDs(t *testing.T) {
	for _, tc := range []struct {
		name      string
		resources []ir.Resource
		want      string
	}{
		{"unknown dependsOn", []ir.Resource{res("a", "ghost")}, `depends on unknown resource "ghost"`},
		{"duplicate id", []ir.Resource{res("a"), res("a")}, `duplicate resource id "a"`},
		{"empty id", []ir.Resource{{Type: "package"}}, "has no id"},
		{
			name: "unknown notifies",
			resources: []ir.Resource{{
				ID: "a", Type: "file",
				Notifies: []ir.Notification{{Resource: "ghost", Action: "restart"}},
			}},
			want: `notifies unknown resource "ghost"`,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := Build(tc.resources)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want it to contain %q", err, tc.want)
			}
		})
	}
}

// TestEdgeKindsAreDistinguishedBetweenTheSamePair is the case the design plan
// calls out: dependsOn and notifies between one pair of resources are two
// different relations and must not collapse into one edge.
func TestEdgeKindsAreDistinguishedBetweenTheSamePair(t *testing.T) {
	conf := ir.Resource{
		ID: "nginx_conf", Type: "file",
		DependsOn: []ir.Dependency{{Resource: "nginx_svc"}},
		Notifies:  []ir.Notification{{Resource: "nginx_svc", On: "changed", Action: "restart"}},
	}
	g := mustBuild(t, ir.Resource{ID: "nginx_svc", Type: "service"}, conf)

	if got := len(g.Edges()); got != 2 {
		t.Fatalf("got %d edges, want 2", got)
	}
	hard := g.EdgesOfKind(HardOrder)
	if len(hard) != 1 || hard[0].From != "nginx_svc" || hard[0].To != "nginx_conf" {
		t.Fatalf("hard edges = %+v, want svc -> conf", hard)
	}
	notify := g.EdgesOfKind(Notify)
	if len(notify) != 1 || notify[0].From != "nginx_conf" || notify[0].To != "nginx_svc" {
		t.Fatalf("notify edges = %+v, want conf -> svc", notify)
	}
	if notify[0].Action != "restart" {
		t.Errorf("notify action = %q, want restart", notify[0].Action)
	}
	if between := g.EdgesBetween("nginx_conf", "nginx_svc"); len(between) != 1 || between[0].Kind != Notify {
		t.Errorf("EdgesBetween(conf, svc) = %+v, want one notify edge", between)
	}
	if between := g.EdgesBetween("nginx_svc", "nginx_conf"); len(between) != 1 || between[0].Kind != HardOrder {
		t.Errorf("EdgesBetween(svc, conf) = %+v, want one dependsOn edge", between)
	}
}

// TestNotifyEdgesDoNotConstrainOrder guards the semantic distinction: notifies
// is a deferred trigger, so a notification loop must still sort.
func TestNotifyEdgesDoNotConstrainOrder(t *testing.T) {
	a := ir.Resource{ID: "a", Type: "service", Notifies: []ir.Notification{{Resource: "b", Action: "restart"}}}
	b := ir.Resource{ID: "b", Type: "service", Notifies: []ir.Notification{{Resource: "a", Action: "restart"}}}
	g := mustBuild(t, a, b)
	order, err := g.TopoSort()
	if err != nil {
		t.Fatalf("TopoSort: %v, want a notify loop to be legal", err)
	}
	if len(order) != 2 || order[0] != "a" || order[1] != "b" {
		t.Errorf("order = %v, want declaration order", order)
	}
}

func TestEdgeKindString(t *testing.T) {
	if HardOrder.String() != "dependsOn" || Notify.String() != "notifies" {
		t.Errorf("kind names = %q, %q", HardOrder, Notify)
	}
	if got := EdgeKind(99).String(); !strings.Contains(got, "99") {
		t.Errorf("unknown kind = %q", got)
	}
}

func TestDependenciesAndDependents(t *testing.T) {
	g := mustBuild(t, res("a"), res("b"), res("c", "b", "a"))
	deps := g.Dependencies("c")
	if len(deps) != 2 || deps[0] != "a" || deps[1] != "b" {
		t.Errorf("Dependencies(c) = %v, want [a b] in declaration order", deps)
	}
	if got := g.Dependents("a"); len(got) != 1 || got[0] != "c" {
		t.Errorf("Dependents(a) = %v, want [c]", got)
	}
	if got := g.Dependents("c"); len(got) != 0 {
		t.Errorf("Dependents(c) = %v, want none", got)
	}
}

func TestNodeLookup(t *testing.T) {
	g := mustBuild(t, res("a"), res("b"))
	n, ok := g.Node("b")
	if !ok || n.Index != 1 {
		t.Fatalf("Node(b) = %+v, %v", n, ok)
	}
	if _, ok := g.Node("ghost"); ok {
		t.Error("Node(ghost) reported found")
	}
}

func TestBuildDocumentUsesDocumentResources(t *testing.T) {
	docs, err := ir.Parse("rs.yaml", []byte(`apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: web}
spec:
  target: ansible
  resources:
    - {id: nginx_pkg, type: package}
    - id: nginx_conf
      type: file
      dependsOn: [{resource: nginx_pkg}]
      notifies: [{resource: nginx_svc, on: changed, action: restart}]
    - id: nginx_svc
      type: service
      dependsOn: [{resource: nginx_pkg}]
`))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if err := ir.Validate(docs[0]); err != nil {
		t.Fatalf("Validate: %v", err)
	}
	g, err := BuildDocument(docs[0])
	if err != nil {
		t.Fatalf("BuildDocument: %v", err)
	}
	order := mustSort(t, g)
	want := []string{"nginx_pkg", "nginx_conf", "nginx_svc"}
	for i := range want {
		if order[i] != want[i] {
			t.Fatalf("order = %v, want %v", order, want)
		}
	}
	if len(g.EdgesOfKind(Notify)) != 1 {
		t.Errorf("notify edges = %+v, want one", g.EdgesOfKind(Notify))
	}
	// Positions survive from the parser through graph construction, so a later
	// stage can point at the source line that declared an edge.
	if g.EdgesOfKind(Notify)[0].Pos.Line == 0 {
		t.Error("notify edge lost its source position")
	}
}

func TestErrorFormatting(t *testing.T) {
	e := &Error{Msg: "boom", Pos: ir.Position{File: "a.yaml", Line: 3, Column: 5}, Cycle: []string{"a", "b", "a"}}
	got := e.Error()
	for _, want := range []string{"a.yaml:3:5", "boom", "a -> b -> a"} {
		if !strings.Contains(got, want) {
			t.Errorf("error = %q, want it to contain %q", got, want)
		}
	}
	if got := (&Error{Msg: "plain"}).Error(); got != "plain" {
		t.Errorf("error = %q, want %q", got, "plain")
	}
}
