package ast

import (
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

func toSDK(t *testing.T, resources ...ir.Resource) *sdk.ResourceGraph {
	t.Helper()
	for i := range resources {
		resources[i].Index = i
	}
	g, err := Build(&ir.ResourceSet{
		APIVersion: ir.APIVersionV1,
		Kind:       ir.KindResourceSet,
		Metadata:   ir.Metadata{Name: "web"},
		Spec:       ir.ResourceSetSpec{Target: "ansible", Resources: resources},
	}, "web01.example.com")
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	return g.ToSDK()
}

// TestToSDKCarriesEveryFieldATargetNeeds is the fidelity check on the one seam
// between the compiler's tree and the public one: a field dropped here would
// silently deprive every target of it.
func TestToSDKCarriesEveryFieldATargetNeeds(t *testing.T) {
	g := toSDK(t,
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"},
			Pos: ir.Position{File: "site.yaml", DocIdx: 2, Line: 7, Column: 3}},
		ir.Resource{ID: "conf", Type: "file", State: "present",
			Params:      map[string]any{"path": "/etc/n.conf", "mode": "0644"},
			RuntimeWhen: "ansible_facts['os_family'] == 'Debian'",
			DependsOn:   []ir.Dependency{{Resource: "pkg"}},
			Notifies:    []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "svc", Type: "service", State: "running",
			Params: map[string]any{"name": "nginx"}},
	)

	if g.Name != "web" || g.Host != "web01.example.com" || g.Target != "ansible" {
		t.Fatalf("graph identity = %+v", g)
	}

	pkg, ok := g.Resource("pkg")
	if !ok {
		t.Fatal("pkg did not survive the conversion")
	}
	if pkg.Pos != (sdk.Position{File: "site.yaml", DocIdx: 2, Line: 7, Column: 3}) {
		t.Fatalf("position = %+v", pkg.Pos)
	}

	conf, _ := g.Resource("conf")
	if conf.State != "present" || conf.RuntimeWhen == "" {
		t.Fatalf("conf = %+v", conf)
	}
	if conf.Params["mode"] != "0644" {
		t.Fatalf("params = %v", conf.Params)
	}

	// Apply order is the compiler's, not the document's, and the index each
	// resource carries has to stay consistent with the order it arrives in.
	for i, r := range g.Resources {
		if r.Index != i {
			t.Fatalf("resource %q arrived at position %d carrying index %d", r.ID, i, r.Index)
		}
	}
}

func TestToSDKCarriesBothEdgeKinds(t *testing.T) {
	g := toSDK(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}},
			Notifies:  []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}},
	)

	hard := g.EdgesOfKind(sdk.HardOrder)
	if len(hard) != 1 || hard[0].From != "pkg" || hard[0].To != "conf" {
		t.Fatalf("hard-order edges = %+v", hard)
	}
	notify := g.EdgesOfKind(sdk.Notify)
	if len(notify) != 1 || notify[0].From != "conf" || notify[0].To != "svc" || notify[0].Action != "restart" {
		t.Fatalf("notify edges = %+v", notify)
	}
	if len(g.Edges) != 2 {
		t.Fatalf("edges = %+v", g.Edges)
	}
}

// TestToSDKDeepCopiesParams: a compiled-in emitter has no process boundary to
// stop it writing to what it was handed, so the copy is what protects the
// compiler's own tree from a target that rewrites a param.
func TestToSDKDeepCopiesParams(t *testing.T) {
	g, err := Build(&ir.ResourceSet{
		APIVersion: ir.APIVersionV1,
		Kind:       ir.KindResourceSet,
		Metadata:   ir.Metadata{Name: "web"},
		Spec: ir.ResourceSetSpec{Target: "ansible", Resources: []ir.Resource{
			{ID: "user", Type: "user", Params: map[string]any{
				"name":   "deploy",
				"groups": []any{"deploy", "sudo"},
				"limits": map[string]any{"nofile": 4096},
			}},
		}},
	}, "web01.example.com")
	if err != nil {
		t.Fatalf("Build: %v", err)
	}

	out := g.ToSDK()
	params := out.Resources[0].Params
	params["name"] = "tampered"
	params["groups"].([]any)[0] = "tampered"
	params["limits"].(map[string]any)["nofile"] = 1

	original := g.Resources()[0].Params
	if original["name"] != "deploy" {
		t.Fatalf("a target rewrote the compiler's own params: %v", original)
	}
	if original["groups"].([]any)[0] != "deploy" {
		t.Fatalf("the copy was shallow at a slice: %v", original)
	}
	if original["limits"].(map[string]any)["nofile"] != 4096 {
		t.Fatalf("the copy was shallow at a nested map: %v", original)
	}

	// And two conversions of one tree are independent of each other.
	second := g.ToSDK()
	if second.Resources[0].Params["name"] != "deploy" {
		t.Fatalf("one conversion contaminated the next: %v", second.Resources[0].Params)
	}
}

func TestToSDKOfNilIsNil(t *testing.T) {
	var g *ResourceGraph
	if g.ToSDK() != nil {
		t.Fatal("converting a nil tree should give a nil tree, not an empty one")
	}
}

// TestToSDKIsDeterministic: the conversion sits on the path every compile
// takes, so a map iterated in place here would make output vary run to run.
//
// The two canonical forms are deliberately not compared. The SDK form records
// each resource's declaration index and the internal one does not, because a
// target receiving a flat list needs to know where a resource came from while
// the compiler holds that in its own index map.
func TestToSDKIsDeterministic(t *testing.T) {
	resources := []ir.Resource{
		{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx", "port": 8080}},
		{ID: "svc", Type: "service", State: "running", Params: map[string]any{"name": "nginx"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}},
			Notifies:  []ir.Notification{{Resource: "pkg", Action: "restart"}}},
	}
	for i := range resources {
		resources[i].Index = i
	}
	g, err := Build(&ir.ResourceSet{
		APIVersion: ir.APIVersionV1,
		Kind:       ir.KindResourceSet,
		Metadata:   ir.Metadata{Name: "web"},
		Spec:       ir.ResourceSetSpec{Target: "ansible", Resources: resources},
	}, "web01.example.com")
	if err != nil {
		t.Fatalf("Build: %v", err)
	}

	want := g.ToSDK().Canonical()
	for i := 0; i < 50; i++ {
		if got := g.ToSDK().Canonical(); got != want {
			t.Fatalf("conversion %d differed:\n%s", i, got)
		}
	}
	if !strings.Contains(want, "index=0") {
		t.Fatalf("the declaration index did not survive:\n%s", want)
	}
}
