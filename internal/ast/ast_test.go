package ast

import (
	"errors"
	"strings"
	"sync"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

func resourceSet(resources ...ir.Resource) *ir.ResourceSet {
	for i := range resources {
		resources[i].Index = i
	}
	return &ir.ResourceSet{
		APIVersion: ir.APIVersionV1,
		Kind:       ir.KindResourceSet,
		Metadata:   ir.Metadata{Name: "web"},
		Spec: ir.ResourceSetSpec{
			Target:    "ansible",
			Resources: resources,
		},
	}
}

func mustBuild(t *testing.T, rs *ir.ResourceSet) *ResourceGraph {
	t.Helper()
	g, err := Build(rs, "web01")
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	return g
}

func TestBuildOrdersResourcesTopologically(t *testing.T) {
	rs := resourceSet(
		ir.Resource{ID: "svc", Type: "service", DependsOn: []ir.Dependency{{Resource: "pkg"}}},
		ir.Resource{ID: "pkg", Type: "package"},
	)
	got := mustBuild(t, rs).Order()
	want := []string{"pkg", "svc"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("order = %v, want %v", got, want)
	}
}

func TestBuildRejectsAnUnevaluatedWhen(t *testing.T) {
	rs := resourceSet(ir.Resource{ID: "pkg", Type: "package", When: "data.install"})
	_, err := Build(rs, "web01")
	if err == nil {
		t.Fatal("expected an error for a resource that still carries `when`")
	}
	if !strings.Contains(err.Error(), "condition stage") {
		t.Fatalf("error should point at the condition stage, got: %v", err)
	}
}

func TestBuildKeepsRuntimeWhen(t *testing.T) {
	rs := resourceSet(ir.Resource{ID: "pkg", Type: "package", RuntimeWhen: "ansible_os_family == 'Debian'"})
	r, ok := mustBuild(t, rs).Resource("pkg")
	if !ok {
		t.Fatal("resource missing from the tree")
	}
	if r.RuntimeWhen != "ansible_os_family == 'Debian'" {
		t.Fatalf("runtimeWhen = %q, want it preserved", r.RuntimeWhen)
	}
}

func TestBuildRequiresAHost(t *testing.T) {
	_, err := Build(resourceSet(ir.Resource{ID: "pkg", Type: "package"}), "")
	if err == nil || !strings.Contains(err.Error(), "target host") {
		t.Fatalf("expected a missing-host error, got %v", err)
	}
}

func TestBuildRejectsANilDocument(t *testing.T) {
	if _, err := Build(nil, "web01"); err == nil {
		t.Fatal("expected an error for a nil ResourceSet")
	}
}

func TestBuildPropagatesGraphFailures(t *testing.T) {
	rs := resourceSet(ir.Resource{ID: "a", Type: "package", DependsOn: []ir.Dependency{{Resource: "b"}}})
	_, err := Build(rs, "web01")
	if err == nil || !strings.Contains(err.Error(), "unknown resource") {
		t.Fatalf("expected the graph error to surface, got %v", err)
	}
}

func TestBuildPropagatesCycles(t *testing.T) {
	rs := resourceSet(
		ir.Resource{ID: "a", Type: "package", DependsOn: []ir.Dependency{{Resource: "b"}}},
		ir.Resource{ID: "b", Type: "package", DependsOn: []ir.Dependency{{Resource: "a"}}},
	)
	_, err := Build(rs, "web01")
	if err == nil || !strings.Contains(err.Error(), "cycle") {
		t.Fatalf("expected a cycle error, got %v", err)
	}
	var astErr *Error
	if !errors.As(err, &astErr) {
		t.Fatalf("error should be an ast.Error, got %T", err)
	}
}

func TestBuildKeepsBothEdgeKinds(t *testing.T) {
	rs := resourceSet(
		ir.Resource{ID: "conf", Type: "file",
			DependsOn: []ir.Dependency{{Resource: "pkg"}},
			Notifies:  []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "pkg", Type: "package"},
		ir.Resource{ID: "svc", Type: "service"},
	)
	g := mustBuild(t, rs)

	hard := g.EdgesOfKind(HardOrder)
	if len(hard) != 1 || hard[0].From != "pkg" || hard[0].To != "conf" {
		t.Fatalf("hard edges = %+v", hard)
	}
	notify := g.EdgesOfKind(Notify)
	if len(notify) != 1 || notify[0].From != "conf" || notify[0].To != "svc" || notify[0].Action != "restart" {
		t.Fatalf("notify edges = %+v", notify)
	}
	if len(g.Edges()) != 2 {
		t.Fatalf("expected both edges to survive, got %+v", g.Edges())
	}
}

func TestNotifyEdgesDoNotConstrainOrder(t *testing.T) {
	// svc is declared last but notified by conf; notifies is a trigger, not an
	// ordering constraint, so declaration order still decides.
	rs := resourceSet(
		ir.Resource{ID: "conf", Type: "file", Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "svc", Type: "service"},
	)
	if got := mustBuild(t, rs).Order(); got[0] != "conf" {
		t.Fatalf("order = %v, want conf first", got)
	}
}

func TestParamsAreCopiedNotAliased(t *testing.T) {
	params := map[string]any{"name": "nginx", "opts": map[any]any{"retries": 3}}
	rs := resourceSet(ir.Resource{ID: "pkg", Type: "package", Params: params})
	g := mustBuild(t, rs)

	r, _ := g.Resource("pkg")
	r.Params["name"] = "mutated"
	if params["name"] != "nginx" {
		t.Fatal("mutating the AST wrote back into the source document")
	}
	if _, ok := r.Params["opts"].(map[string]any); !ok {
		t.Fatalf("a YAML map[any]any should be normalised, got %T", r.Params["opts"])
	}
}

func TestResourceLookupAndAccessorsCopy(t *testing.T) {
	g := mustBuild(t, resourceSet(ir.Resource{ID: "pkg", Type: "package"}))
	if _, ok := g.Resource("nope"); ok {
		t.Fatal("lookup of an unknown id should report false")
	}
	rs := g.Resources()
	rs[0].ID = "mutated"
	if got := g.Order(); got[0] != "pkg" {
		t.Fatalf("Resources() handed out the internal slice: order = %v", got)
	}
	edges := g.Edges()
	if len(edges) != 0 {
		t.Fatalf("expected no edges, got %v", edges)
	}
}

func TestEdgeKindString(t *testing.T) {
	if HardOrder.String() != "dependsOn" || Notify.String() != "notifies" {
		t.Fatalf("edge kinds render as %q and %q", HardOrder, Notify)
	}
	if got := EdgeKind(7).String(); got != "EdgeKind(7)" {
		t.Fatalf("unknown kind rendered as %q", got)
	}
}

func canonicalFixture() *ir.ResourceSet {
	return resourceSet(
		ir.Resource{ID: "conf", Type: "file",
			Params:    map[string]any{"path": "/etc/n.conf", "mode": "0644", "owner": "root"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}},
			Notifies:  []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
		ir.Resource{ID: "svc", Type: "service", State: "running", Params: map[string]any{"name": "nginx"}},
	)
}

func TestCanonicalIsStableAcrossRebuilds(t *testing.T) {
	want := mustBuild(t, canonicalFixture()).Canonical()
	for i := 0; i < 100; i++ {
		if got := mustBuild(t, canonicalFixture()).Canonical(); got != want {
			t.Fatalf("run %d differed:\n%s\nwant:\n%s", i, got, want)
		}
	}
}

func TestCanonicalIsStableUnderConcurrency(t *testing.T) {
	want := mustBuild(t, canonicalFixture()).Canonical()
	var wg sync.WaitGroup
	got := make([]string, 16)
	for i := range got {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			g, err := Build(canonicalFixture(), "web01")
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			got[i] = g.Canonical()
		}(i)
	}
	wg.Wait()
	for i, g := range got {
		if g != want {
			t.Fatalf("goroutine %d differed:\n%s", i, g)
		}
	}
}

func TestBuildFromBundleResolvesAnInfrastructureOutput(t *testing.T) {
	infra := &ir.Infrastructure{
		APIVersion: ir.APIVersionV1, Kind: ir.KindInfrastructure,
		Metadata: ir.Metadata{Name: "net"},
		Spec: ir.InfrastructureSpec{
			Target:    "terraform",
			Resources: []ir.Resource{{ID: "vm", Type: "vm"}},
			Outputs:   []ir.Output{{Name: "ip", Value: "10.0.0.7"}},
		},
	}
	rs := resourceSet(ir.Resource{ID: "pkg", Type: "package"})
	rs.Spec.TargetHost = &ir.TargetHost{From: ir.TargetHostFromInfrastructure, Ref: "net", Output: "ip"}

	g, err := BuildFromBundle(ir.NewBundle(infra, rs), rs)
	if err != nil {
		t.Fatalf("BuildFromBundle: %v", err)
	}
	if g.Host != "10.0.0.7" {
		t.Fatalf("host = %q, want the resolved output literal", g.Host)
	}
}

func TestBuildFromBundleReportsResolutionFailure(t *testing.T) {
	rs := resourceSet(ir.Resource{ID: "pkg", Type: "package"})
	rs.Spec.TargetHost = &ir.TargetHost{From: ir.TargetHostFromInfrastructure, Ref: "missing", Output: "ip"}
	if _, err := BuildFromBundle(ir.NewBundle(rs), rs); err == nil {
		t.Fatal("expected an error for an unresolvable target host")
	}
	if _, err := BuildFromBundle(nil, rs); err == nil {
		t.Fatal("expected an error for a nil bundle")
	}
}

func TestCanonicalValueRendersNestedShapes(t *testing.T) {
	got := canonicalValue(map[string]any{
		"b": []any{1, "two", true},
		"a": map[any]any{"z": nil},
	})
	want := `{"a":{"z":null},"b":[1,"two",true]}`
	if got != want {
		t.Fatalf("canonicalValue = %s, want %s", got, want)
	}
}

func TestNewAssemblesATreeVerbatim(t *testing.T) {
	// New trusts the caller's order, which is the whole point of it: a stage
	// that has already decided an order must be able to hand one over.
	params := map[string]any{"name": "nginx"}
	g := New("web", "web01", "ansible",
		[]Resource{
			{ID: "svc", Type: "service", Index: 0, Params: params},
			{ID: "pkg", Type: "package", Index: 1},
			{ID: "svc", Type: "service", Index: 2}, // duplicate id, ignored
		},
		[]Edge{
			{From: "pkg", To: "svc", Kind: HardOrder},
			{From: "conf", To: "svc", Kind: Notify, Action: "restart"},
		},
	)

	if got := strings.Join(g.Order(), ","); got != "svc,pkg" {
		t.Fatalf("order = %s, want the caller's own order", got)
	}
	if len(g.Resources()) != 2 {
		t.Fatalf("duplicate id was not dropped: %v", g.Order())
	}
	if g.Host != "web01" || g.Target != "ansible" || g.Name != "web" {
		t.Fatalf("identity = %+v", g)
	}
	if len(g.EdgesOfKind(HardOrder)) != 1 || len(g.EdgesOfKind(Notify)) != 1 {
		t.Fatalf("edges = %+v", g.Edges())
	}
	params["name"] = "mutated"
	r, _ := g.Resource("svc")
	if r.Params["name"] != "nginx" {
		t.Fatal("New should copy params rather than alias the caller's map")
	}
}

func TestEdgesAreOrderedDeterministically(t *testing.T) {
	edges := []Edge{
		{From: "b", To: "a", Kind: Notify, Action: "restart"},
		{From: "a", To: "z", Kind: HardOrder},
		{From: "b", To: "a", Kind: Notify, Action: "reload"},
		{From: "a", To: "b", Kind: HardOrder},
		{From: "b", To: "a", Kind: HardOrder},
	}
	first := New("n", "h", "t", nil, edges).Edges()

	// Shuffling the input must not change the output.
	reversed := make([]Edge, 0, len(edges))
	for i := len(edges) - 1; i >= 0; i-- {
		reversed = append(reversed, edges[i])
	}
	second := New("n", "h", "t", nil, reversed).Edges()

	for i := range first {
		if first[i] != second[i] {
			t.Fatalf("edge %d differs: %+v vs %+v", i, first[i], second[i])
		}
	}
	if first[0].From != "a" || first[0].To != "b" {
		t.Fatalf("edges are not sorted by source: %+v", first)
	}
	if first[3].Action != "reload" || first[4].Action != "restart" {
		t.Fatalf("notify edges are not sorted by action: %+v", first[3:])
	}
}

func TestErrorUnwrapsTheUnderlyingFailure(t *testing.T) {
	inner := errors.New("boom")
	err := &Error{Msg: "wrapped", Err: inner}
	if !errors.Is(err, inner) {
		t.Fatal("Error should unwrap to the failure it wrapped")
	}
	if err.Error() != "wrapped" {
		t.Fatalf("error without a position = %q", err.Error())
	}
	located := &Error{Msg: "wrapped", Pos: ir.Position{File: "site.yaml", Line: 3}}
	if located.Error() != "site.yaml:3: wrapped" {
		t.Fatalf("located error = %q", located.Error())
	}
}
