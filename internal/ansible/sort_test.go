package ansible

import (
	"errors"
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ast"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// TestSortFlattensToAStrictSequence asserts the semantics the design plan's
// per-target table gives Ansible: no native graph, one linear task list.
func TestSortFlattensToAStrictSequence(t *testing.T) {
	files := emit(t,
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"},
			DependsOn: []ir.Dependency{{Resource: "conf"}}},
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}}},
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
	)

	playbook := files[PlaybookPath]
	pkg := strings.Index(playbook, "name: pkg")
	conf := strings.Index(playbook, "name: conf")
	svc := strings.Index(playbook, "name: svc")
	if !(pkg < conf && conf < svc) {
		t.Fatalf("tasks are not in dependency order:\n%s", playbook)
	}
	// Nothing target-native records the graph: no ordering metaparameter of any
	// kind survives into the output, unlike Puppet's require or DSC's DependsOn.
	for _, native := range []string{"require", "before", "DependsOn", "subscribe"} {
		if strings.Contains(playbook, native) {
			t.Fatalf("graph structure leaked into a flattened target via %q:\n%s", native, playbook)
		}
	}
}

func TestSortWarnsWhenParallelismIsLost(t *testing.T) {
	_, warnings, err := New().Emit(buildAST(t,
		ir.Resource{ID: "pkg_a", Type: "package", Params: map[string]any{"name": "nginx"}},
		ir.Resource{ID: "pkg_b", Type: "package", Params: map[string]any{"name": "curl"}},
	), nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	if len(warnings) != 1 {
		t.Fatalf("warnings = %v, want exactly one", warnings)
	}
	w := warnings[0]
	if w.Target != Name {
		t.Fatalf("warning target = %q", w.Target)
	}
	for _, id := range []string{"pkg_a", "pkg_b"} {
		if !strings.Contains(w.String(), id) {
			t.Fatalf("warning should name the independent resources, got %q", w.String())
		}
	}
	if !strings.Contains(w.Msg, "parallelism") {
		t.Fatalf("warning should say what was lost, got %q", w.Msg)
	}
}

func TestSortDoesNotWarnWhenTheGraphIsAlreadyLinear(t *testing.T) {
	_, warnings, err := New().Emit(buildAST(t,
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"},
			DependsOn: []ir.Dependency{{Resource: "pkg"}}},
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
	), nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	if len(warnings) != 0 {
		t.Fatalf("a fully ordered graph loses nothing, got warnings %v", warnings)
	}
}

func TestNotifyEdgesDoNotAffectTheFlattenedOrder(t *testing.T) {
	files := emit(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}},
	)
	playbook := files[PlaybookPath]
	if strings.Index(playbook, "name: conf") > strings.Index(playbook, "name: svc") {
		t.Fatalf("a notify edge reordered the play:\n%s", playbook)
	}
}

// TestSortRejectsAnOrderItCannotFlatten is defensive: the graph stage should
// make this impossible, but a target that assumes an invariant it never checks
// fails quietly when the assumption breaks.
func TestSortRejectsAnOrderItCannotFlatten(t *testing.T) {
	// "b" depends on "a" but is listed first, an order Build would never emit.
	g := ast.New("web", "web01.example.com", Name,
		[]ast.Resource{
			{ID: "b", Type: "package", Params: map[string]any{"name": "b"}, Index: 0},
			{ID: "a", Type: "package", Params: map[string]any{"name": "a"}, Index: 1},
		},
		[]ast.Edge{{From: "a", To: "b", Kind: ast.HardOrder}},
	)
	_, _, err := sortTasks(g, []task{{Name: "b"}, {Name: "a"}})
	var e *Error
	if !errors.As(err, &e) || e.Rule != "ordering-violated" {
		t.Fatalf("error = %v, want an ordering violation", err)
	}
	if !strings.Contains(err.Error(), `"b"`) || !strings.Contains(err.Error(), `"a"`) {
		t.Fatalf("error should name both resources: %v", err)
	}
}

// TestSortRejectsACycleReachingIt covers the same defence for a cycle, which
// leaves resources that never become ready.
func TestSortRejectsACycleReachingIt(t *testing.T) {
	g := ast.New("web", "web01.example.com", Name,
		[]ast.Resource{
			{ID: "a", Type: "package", Params: map[string]any{"name": "a"}, Index: 0},
			{ID: "b", Type: "package", Params: map[string]any{"name": "b"}, Index: 1},
		},
		[]ast.Edge{
			{From: "a", To: "b", Kind: ast.HardOrder},
			{From: "b", To: "a", Kind: ast.HardOrder},
		},
	)
	_, _, err := sortTasks(g, []task{{Name: "a"}, {Name: "b"}})
	var e *Error
	if !errors.As(err, &e) || e.Rule != "cycle" {
		t.Fatalf("error = %v, want a cycle error", err)
	}
}
