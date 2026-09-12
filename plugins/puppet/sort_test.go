package puppet

import (
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// TestOrderingSurvivesAsRequireMetaparameters is the claim that distinguishes
// this target from Ansible's. Ansible has nowhere to put a graph, so its sort
// stage flattens one into a task list and warns about what that cost. Puppet's
// catalog is a graph, so each edge the author wrote appears in the output as the
// require that means the same thing.
func TestOrderingSurvivesAsRequireMetaparameters(t *testing.T) {
	m := compiled(t,
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"},
			DependsOn: []ir.Dependency{{Resource: "conf"}}},
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}}},
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
	)

	want := map[string][]string{
		"pkg":  nil,
		"conf": {"Package['pkg']"},
		"svc":  {"File['conf']"},
	}
	for title, refs := range want {
		got := refStrings(declared(t, m, title).Require)
		if strings.Join(got, ",") != strings.Join(refs, ",") {
			t.Errorf("%s requires %v, want %v", title, got, refs)
		}
	}
}

// TestNothingIsFlattenedAndNothingIsWarnedAbout is the other half of the
// contrast. Two independent resources cost Ansible a warning because it has to
// serialise them; here they simply carry no require and the agent may apply them
// in either order.
func TestNothingIsFlattenedAndNothingIsWarnedAbout(t *testing.T) {
	g := buildAST(t,
		ir.Resource{ID: "pkg_a", Type: "package", Params: map[string]any{"name": "nginx"}},
		ir.Resource{ID: "pkg_b", Type: "package", Params: map[string]any{"name": "curl"}},
	)
	_, warnings, err := New().Emit(g, nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	if len(warnings) != 0 {
		t.Fatalf("a target that preserves the graph has nothing to warn about, got %v", warnings)
	}

	m := compiled(t,
		ir.Resource{ID: "pkg_a", Type: "package", Params: map[string]any{"name": "nginx"}},
		ir.Resource{ID: "pkg_b", Type: "package", Params: map[string]any{"name": "curl"}},
	)
	for _, r := range m.Resources {
		if len(r.Require) != 0 {
			t.Errorf("independent resource %q acquired %v", r.Title, refStrings(r.Require))
		}
	}
}

// TestOneResourceCollectsEveryDependencyItDeclared checks a fan-in: three edges
// arriving at one resource become three references on it, sorted so the output
// does not depend on the order the graph stage happened to emit them in.
func TestOneResourceCollectsEveryDependencyItDeclared(t *testing.T) {
	m := compiled(t,
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "app"}},
		ir.Resource{ID: "dir", Type: "file", State: "directory", Params: map[string]any{"path": "/srv/app"}},
		ir.Resource{ID: "acct", Type: "user", Params: map[string]any{"name": "app"}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "app"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}, {Resource: "dir"}, {Resource: "acct"}}},
	)

	got := refStrings(declared(t, m, "svc").Require)
	want := []string{"File['dir']", "Package['pkg']", "User['acct']"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("svc requires %v, want %v", got, want)
	}
}

// TestANotifiedDependencyIsNotAlsoRequired keeps the output honest. A notify
// already orders the pair, so writing require as well would be two ways of
// saying one thing and would read as though the author asked for both.
func TestANotifiedDependencyIsNotAlsoRequired(t *testing.T) {
	m := compiled(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/app.conf"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "app"},
			DependsOn: []ir.Dependency{{Resource: "conf"}}},
	)

	conf := declared(t, m, "conf")
	if got := refStrings(conf.Notify); strings.Join(got, ",") != "Service['svc']" {
		t.Fatalf("conf notifies %v", got)
	}
	if got := refStrings(declared(t, m, "svc").Require); len(got) != 0 {
		t.Fatalf("svc requires %v, but the notify on conf already orders the pair", got)
	}
}

// TestARequireIsNotWrittenTwiceForTheSamePair guards the deduplication directly,
// since the graph stage is free to produce more than one edge between a pair.
func TestARequireIsNotWrittenTwiceForTheSamePair(t *testing.T) {
	g := buildAST(t,
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "app"}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "app"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}}},
	)
	g.Edges = append(g.Edges, sdk.Edge{From: "pkg", To: "svc", Kind: sdk.HardOrder})

	m, err := transform(g)
	if err != nil {
		t.Fatalf("transform: %v", err)
	}
	if _, err := applyOrdering(g, m); err != nil {
		t.Fatalf("applyOrdering: %v", err)
	}
	if got := refStrings(declared(t, m, "svc").Require); len(got) != 1 {
		t.Fatalf("svc requires %v, want one reference", got)
	}
}

// TestACycleIsRefusedRatherThanShippedToThePrimary covers the check this stage
// makes of its own input. Puppet would catch the cycle too, but on the
// operator's primary and in terms of resource titles rather than the document.
func TestACycleIsRefusedRatherThanShippedToThePrimary(t *testing.T) {
	g := buildAST(t,
		ir.Resource{ID: "a", Type: "package", Params: map[string]any{"name": "a"}},
		ir.Resource{ID: "b", Type: "package", Params: map[string]any{"name": "b"}},
	)
	g.Edges = []sdk.Edge{
		{From: "a", To: "b", Kind: sdk.HardOrder},
		{From: "b", To: "a", Kind: sdk.HardOrder},
	}

	m, err := transform(g)
	if err != nil {
		t.Fatalf("transform: %v", err)
	}
	_, err = applyOrdering(g, m)
	if err == nil {
		t.Fatal("a cycle reached the serialiser")
	}
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "cycle" {
		t.Fatalf("rule = %q, want cycle", ce.Rule)
	}
	if !strings.Contains(ce.Msg, "graph stage") {
		t.Fatalf("the message should say which stage should have caught it: %q", ce.Msg)
	}
}

// TestAnOrderThatContradictsAnEdgeIsRefused covers the second self-check: a
// topological order that places a resource before something it depends on would
// silently produce a manifest whose requires disagree with it.
func TestAnOrderThatContradictsAnEdgeIsRefused(t *testing.T) {
	g := buildAST(t,
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "app"}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "app"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}}},
	)
	// Reverse the order without touching the edges, which is exactly the
	// disagreement the stage is looking for.
	for i, j := 0, len(g.Resources)-1; i < j; i, j = i+1, j-1 {
		g.Resources[i], g.Resources[j] = g.Resources[j], g.Resources[i]
	}

	m, err := transform(g)
	if err != nil {
		t.Fatalf("transform: %v", err)
	}
	_, err = applyOrdering(g, m)
	if err == nil {
		t.Fatal("an order contradicting its own edges was accepted")
	}
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "ordering-violated" {
		t.Fatalf("rule = %q, want ordering-violated", ce.Rule)
	}
	if !strings.Contains(ce.Msg, "pkg") || ce.Resource != "svc" {
		t.Fatalf("the failure should name both ends of the edge: %v", ce)
	}
}

// refStrings renders a resource's metaparameter list the way the manifest will,
// so an assertion reads as the Puppet an operator would see.
func refStrings(refs []reference) []string {
	out := make([]string, 0, len(refs))
	for _, r := range refs {
		out = append(out, r.String())
	}
	return out
}
