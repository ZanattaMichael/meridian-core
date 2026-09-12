package ansible

import (
	"errors"
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// validateCase runs only the validate stage against a hand-built playbook, so a
// constraint can be exercised even when transform would never produce it.
func validateCase(t *testing.T, pb *playbook) *Error {
	t.Helper()
	err := validate(pb)
	if err == nil {
		t.Fatal("expected validation to fail")
	}
	var e *Error
	if !errors.As(err, &e) {
		t.Fatalf("error should be an ansible.Error, got %T: %v", err, err)
	}
	return e
}

func okTask(name string) task {
	return task{Name: name, ResourceID: name, Module: "ansible.builtin.package",
		Args: map[string]any{"name": name, "state": "present"}}
}

func TestValidateRequiresAHost(t *testing.T) {
	e := validateCase(t, &playbook{Name: "web", Tasks: []task{okTask("pkg")}})
	if e.Rule != "missing-host" {
		t.Fatalf("rule = %q", e.Rule)
	}
}

func TestValidateRejectsDuplicateTaskNames(t *testing.T) {
	// Handlers are notified by task name, so a duplicate is ambiguous rather
	// than merely untidy.
	e := validateCase(t, &playbook{Name: "web", Hosts: "web01",
		Tasks: []task{okTask("pkg"), okTask("pkg")}})
	if e.Rule != "duplicate-task-name" {
		t.Fatalf("rule = %q", e.Rule)
	}
	if e.Resource != "pkg" || !strings.Contains(e.Error(), `"pkg"`) {
		t.Fatalf("error should name the offending resource: %v", e)
	}
}

func TestValidateRejectsDuplicateHandlerNames(t *testing.T) {
	h := okTask("restart svc")
	e := validateCase(t, &playbook{Name: "web", Hosts: "web01",
		Tasks: []task{okTask("pkg")}, Handlers: []task{h, h}})
	if e.Rule != "duplicate-handler-name" {
		t.Fatalf("rule = %q", e.Rule)
	}
}

func TestValidateRejectsAnEmptyName(t *testing.T) {
	e := validateCase(t, &playbook{Name: "web", Hosts: "web01", Tasks: []task{okTask("  ")}})
	if e.Rule != "empty-name" {
		t.Fatalf("rule = %q", e.Rule)
	}
}

func TestValidateRejectsAMultilineName(t *testing.T) {
	e := validateCase(t, &playbook{Name: "web", Hosts: "web01", Tasks: []task{okTask("a\nb")}})
	if e.Rule != "multiline-name" {
		t.Fatalf("rule = %q", e.Rule)
	}
}

func TestValidateRejectsANotifyWithNoHandler(t *testing.T) {
	pkg := okTask("pkg")
	pkg.Notify = []string{"restart svc"}
	e := validateCase(t, &playbook{Name: "web", Hosts: "web01", Tasks: []task{pkg}})
	if e.Rule != "missing-handler" {
		t.Fatalf("rule = %q", e.Rule)
	}
	if !strings.Contains(e.Error(), "restart svc") {
		t.Fatalf("error should name the handler: %v", e)
	}
}

func TestValidateRejectsAConditionalHandler(t *testing.T) {
	h := okTask("restart svc")
	h.When = "something"
	e := validateCase(t, &playbook{Name: "web", Hosts: "web01",
		Tasks: []task{okTask("pkg")}, Handlers: []task{h}})
	if e.Rule != "conditional-handler" {
		t.Fatalf("rule = %q", e.Rule)
	}
}

func TestValidateRejectsAModulelessOrArgumentlessTask(t *testing.T) {
	bare := task{Name: "pkg", ResourceID: "pkg"}
	if e := validateCase(t, &playbook{Name: "web", Hosts: "web01", Tasks: []task{bare}}); e.Rule != "missing-module" {
		t.Fatalf("rule = %q", e.Rule)
	}
	empty := task{Name: "pkg", ResourceID: "pkg", Module: "ansible.builtin.package"}
	if e := validateCase(t, &playbook{Name: "web", Hosts: "web01", Tasks: []task{empty}}); e.Rule != "empty-module-args" {
		t.Fatalf("rule = %q", e.Rule)
	}
}

func TestValidateAcceptsAWellFormedPlay(t *testing.T) {
	pkg := okTask("pkg")
	pkg.Notify = []string{"restart svc"}
	pb := &playbook{Name: "web", Hosts: "web01",
		Tasks: []task{pkg}, Handlers: []task{okTask("restart svc")}}
	if err := validate(pb); err != nil {
		t.Fatalf("validate: %v", err)
	}
}

func TestEmitRejectsANilTree(t *testing.T) {
	if _, _, err := New().Emit(nil, nil); err == nil {
		t.Fatal("expected an error for a nil tree")
	}
}

func TestCompileErrorsNameTheResourceAndRule(t *testing.T) {
	// Every failure an author can cause must be traceable to the line they
	// wrote, so each carries both a resource id and a named rule.
	err := emitError(t, ir.Resource{ID: "bad_pkg", Type: "package",
		Params: map[string]any{"name": "nginx", "nope": 1}})
	var e *Error
	if !errors.As(err, &e) {
		t.Fatalf("error type %T", err)
	}
	if e.Resource != "bad_pkg" || e.Rule == "" {
		t.Fatalf("error = %+v", e)
	}
}
