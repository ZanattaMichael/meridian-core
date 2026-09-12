package ansible

import (
	"errors"
	"fmt"
	"reflect"
	"sort"
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// TestEverySupportedTypeIsMapped is the coverage guard the testing plan asks
// for: every entry in SupportedResourceTypes has a case here, and the test
// fails when a new type is added without one.
func TestEverySupportedTypeIsMapped(t *testing.T) {
	cases := map[string]struct {
		resource   ir.Resource
		wantModule string
		wantArgs   map[string]any
	}{
		"package": {
			resource:   ir.Resource{ID: "r", Type: "package", Params: map[string]any{"name": "nginx"}},
			wantModule: "ansible.builtin.package",
			wantArgs:   map[string]any{"name": "nginx", "state": "present"},
		},
		"service": {
			resource: ir.Resource{ID: "r", Type: "service", State: "running",
				Params: map[string]any{"name": "nginx", "enabled": true}},
			wantModule: "ansible.builtin.service",
			wantArgs:   map[string]any{"name": "nginx", "enabled": true, "state": "started"},
		},
		"file": {
			resource: ir.Resource{ID: "r", Type: "file",
				Params: map[string]any{"path": "/etc/n.conf", "owner": "root", "mode": "0644"}},
			wantModule: "ansible.builtin.file",
			wantArgs:   map[string]any{"path": "/etc/n.conf", "owner": "root", "mode": "0644", "state": "file"},
		},
		"user": {
			resource: ir.Resource{ID: "r", Type: "user",
				Params: map[string]any{"name": "deploy", "shell": "/bin/bash", "uid": 1200}},
			wantModule: "ansible.builtin.user",
			wantArgs:   map[string]any{"name": "deploy", "shell": "/bin/bash", "uid": 1200, "state": "present"},
		},
		"group": {
			resource: ir.Resource{ID: "r", Type: "group",
				Params: map[string]any{"name": "deploy", "gid": 1200}},
			wantModule: "ansible.builtin.group",
			wantArgs:   map[string]any{"name": "deploy", "gid": 1200, "state": "present"},
		},
		"exec": {
			resource: ir.Resource{ID: "r", Type: "exec",
				Params: map[string]any{"command": "/usr/bin/true", "creates": "/tmp/done"}},
			wantModule: "ansible.builtin.command",
			wantArgs:   map[string]any{"cmd": "/usr/bin/true", "creates": "/tmp/done"},
		},
	}

	for _, typ := range SupportedResourceTypes() {
		if _, ok := cases[typ]; !ok {
			t.Errorf("resource type %q is supported but has no mapping test", typ)
		}
	}

	for typ, tc := range cases {
		t.Run(typ, func(t *testing.T) {
			got := transformed(t, tc.resource).Tasks[0]
			if got.Module != tc.wantModule {
				t.Fatalf("module = %q, want %q", got.Module, tc.wantModule)
			}
			if !reflect.DeepEqual(got.Args, tc.wantArgs) {
				t.Fatalf("args = %#v, want %#v", got.Args, tc.wantArgs)
			}
		})
	}
}

func TestFileWithContentBecomesACopy(t *testing.T) {
	got := transformed(t, ir.Resource{ID: "r", Type: "file",
		Params: map[string]any{"path": "/etc/n.conf", "content": "hello\n"}}).Tasks[0]
	if got.Module != "ansible.builtin.copy" {
		t.Fatalf("module = %q, want the copy module for literal content", got.Module)
	}
	if got.Args["dest"] != "/etc/n.conf" {
		t.Fatalf("copy should use dest, got args %#v", got.Args)
	}
	if _, ok := got.Args["state"]; ok {
		t.Fatalf("copy implies its own state and must not emit one: %#v", got.Args)
	}
}

func TestFileWithContentRejectsAContradictoryState(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "r", Type: "file", State: "directory",
		Params: map[string]any{"path": "/etc/n.d", "content": "hello\n"}})
	if !strings.Contains(err.Error(), "cannot have state") {
		t.Fatalf("error = %v", err)
	}
}

// TestStateVocabularyIsExhaustive walks every accepted state value of every
// type, so a state that maps to nothing cannot slip in unnoticed.
func TestStateVocabularyIsExhaustive(t *testing.T) {
	params := map[string]map[string]any{
		"package": {"name": "nginx"},
		"service": {"name": "nginx"},
		"file":    {"path": "/etc/n.conf"},
		"user":    {"name": "deploy"},
		"group":   {"name": "deploy"},
	}
	for typ, m := range mappings {
		if m.stateArg == "" && m.stateArgFor == nil {
			continue
		}
		for state, want := range m.states {
			t.Run(typ+"/"+state, func(t *testing.T) {
				got := transformed(t, ir.Resource{ID: "r", Type: typ, State: state, Params: params[typ]}).Tasks[0]
				if got.Args["state"] != want {
					t.Fatalf("state %q mapped to %v, want %q", state, got.Args["state"], want)
				}
			})
		}
	}
}

func TestDefaultStateIsAppliedWhenNoneIsDeclared(t *testing.T) {
	got := transformed(t, ir.Resource{ID: "r", Type: "service", Params: map[string]any{"name": "nginx"}}).Tasks[0]
	if got.Args["state"] != "started" {
		t.Fatalf("state = %v, want the default running/started", got.Args["state"])
	}
}

func TestOptionalParamsAreOmittedRatherThanDefaulted(t *testing.T) {
	got := transformed(t, ir.Resource{ID: "r", Type: "user", Params: map[string]any{"name": "deploy"}}).Tasks[0]
	for _, arg := range []string{"uid", "shell", "home", "groups", "system"} {
		if _, ok := got.Args[arg]; ok {
			t.Fatalf("undeclared optional param %q was invented: %#v", arg, got.Args)
		}
	}
}

func TestUnsupportedResourceTypeIsASpecificError(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "r", Type: "windows_registry_key",
		Params: map[string]any{"path": "HKLM:\\Software"}})

	var e *Error
	if !errors.As(err, &e) {
		t.Fatalf("error should be an ansible.Error, got %T: %v", err, err)
	}
	if e.Rule != "unsupported-resource-type" {
		t.Fatalf("rule = %q, want unsupported-resource-type", e.Rule)
	}
	if e.Resource != "r" {
		t.Fatalf("error should name the resource, got %q", e.Resource)
	}
	for _, typ := range SupportedResourceTypes() {
		if !strings.Contains(err.Error(), typ) {
			t.Fatalf("error should list the supported types, missing %q: %v", typ, err)
		}
	}
}

func TestUnsupportedParamIsRejected(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "r", Type: "package",
		Params: map[string]any{"name": "nginx", "provider": "apt"}})
	var e *Error
	if !errors.As(err, &e) || e.Rule != "unsupported-param" {
		t.Fatalf("error = %v", err)
	}
	if !strings.Contains(err.Error(), `"provider"`) {
		t.Fatalf("error should name the param, got %v", err)
	}
}

func TestMissingRequiredParamIsRejected(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "r", Type: "package"})
	var e *Error
	if !errors.As(err, &e) || e.Rule != "missing-param" {
		t.Fatalf("error = %v", err)
	}
}

func TestEmptyParamValueIsRejected(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "r", Type: "package", Params: map[string]any{"name": nil}})
	var e *Error
	if !errors.As(err, &e) || e.Rule != "empty-param" {
		t.Fatalf("error = %v", err)
	}
}

func TestUnknownStateIsRejectedWithTheAcceptedValues(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "r", Type: "package", State: "installed",
		Params: map[string]any{"name": "nginx"}})
	var e *Error
	if !errors.As(err, &e) || e.Rule != "unsupported-state" {
		t.Fatalf("error = %v", err)
	}
	for _, state := range []string{"absent", "latest", "present"} {
		if !strings.Contains(err.Error(), state) {
			t.Fatalf("error should list accepted state %q: %v", state, err)
		}
	}
}

func TestExecRejectsAState(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "r", Type: "exec", State: "present",
		Params: map[string]any{"command": "/usr/bin/true"}})
	if !strings.Contains(err.Error(), "accepts no state") {
		t.Fatalf("error = %v", err)
	}
}

func TestExecShellParamSelectsTheShellModule(t *testing.T) {
	got := transformed(t, ir.Resource{ID: "r", Type: "exec",
		Params: map[string]any{"command": "a | b", "shell": true}}).Tasks[0]
	if got.Module != "ansible.builtin.shell" {
		t.Fatalf("module = %q, want the shell module", got.Module)
	}
	if _, ok := got.Args["shell"]; ok {
		t.Fatalf("the steering param must not become an argument: %#v", got.Args)
	}
}

func TestNotifyCompilesToHandlersAndIsDeduped(t *testing.T) {
	pb := transformed(t,
		ir.Resource{ID: "conf_a", Type: "file", Params: map[string]any{"path": "/etc/a"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "conf_b", Type: "file", Params: map[string]any{"path": "/etc/b"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}},
	)

	if len(pb.Handlers) != 1 {
		t.Fatalf("two notifiers of the same change should share one handler, got %d", len(pb.Handlers))
	}
	h := pb.Handlers[0]
	if h.Name != "restart svc" {
		t.Fatalf("handler name = %q", h.Name)
	}
	if h.Args["state"] != "restarted" {
		t.Fatalf("handler should apply the action, got args %#v", h.Args)
	}
	for _, id := range []string{"conf_a", "conf_b"} {
		task := taskNamed(t, pb, id)
		if len(task.Notify) != 1 || task.Notify[0] != "restart svc" {
			t.Fatalf("%s notify = %v", id, task.Notify)
		}
	}
	// The notified service is still applied in its own right.
	if svc := taskNamed(t, pb, "svc"); svc.Args["state"] != "started" {
		t.Fatalf("the notified resource lost its own state: %#v", svc.Args)
	}
}

func TestDistinctActionsOnOneResourceBecomeDistinctHandlers(t *testing.T) {
	pb := transformed(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/a"},
			Notifies: []ir.Notification{
				{Resource: "svc", Action: "restart"},
				{Resource: "svc", Action: "reload"},
			}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}},
	)
	var names []string
	for _, h := range pb.Handlers {
		names = append(names, h.Name)
	}
	sort.Strings(names)
	if fmt.Sprint(names) != "[reload svc restart svc]" {
		t.Fatalf("handlers = %v", names)
	}
}

func TestUnsupportedNotifyActionIsRejected(t *testing.T) {
	err := emitError(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/a"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "rotate"}}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}},
	)
	var e *Error
	if !errors.As(err, &e) || e.Rule != "unsupported-action" {
		t.Fatalf("error = %v", err)
	}
	if !strings.Contains(err.Error(), "restart") {
		t.Fatalf("error should list the supported actions: %v", err)
	}
}

func TestNotifyingAResourceThatCannotBeNotifiedIsRejected(t *testing.T) {
	err := emitError(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/a"},
			Notifies: []ir.Notification{{Resource: "pkg", Action: "restart"}}},
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
	)
	if !strings.Contains(err.Error(), "cannot be notified") {
		t.Fatalf("error = %v", err)
	}
}

func TestHandlerDropsTheNotifiersRuntimeCondition(t *testing.T) {
	pb := transformed(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/a"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"},
			RuntimeWhen: "ansible_facts['os_family'] == 'Debian'"},
	)
	if pb.Handlers[0].When != "" {
		t.Fatalf("handler kept a condition: %q", pb.Handlers[0].When)
	}
	if taskNamed(t, pb, "svc").When == "" {
		t.Fatal("the resource's own task should keep its runtime condition")
	}
}

func TestErrorRendersPositionAndRule(t *testing.T) {
	e := &Error{Resource: "r", Rule: "unsupported-param", Msg: "nope",
		Pos: ir.Position{File: "site.yaml", Line: 12, Column: 3}}
	want := `site.yaml:12:3: ansible: resource "r" [unsupported-param]: nope`
	if e.Error() != want {
		t.Fatalf("error = %q, want %q", e.Error(), want)
	}
	bare := &Error{Msg: "nope"}
	if bare.Error() != "ansible: nope" {
		t.Fatalf("bare error = %q", bare.Error())
	}
}
