package ansible

import (
	"sort"
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// TestCapabilitiesAreSelfConsistent is the meta-test the testing plan requires
// of every target: a declared capability has to be backed by behaviour, and a
// capability declared false has to produce a documented hard failure rather than
// output that quietly does less.
func TestCapabilitiesAreSelfConsistent(t *testing.T) {
	e := New()
	caps := e.Capabilities()

	if caps.NativeNotify && caps.SyntheticNotify {
		t.Fatal("a target declares one notify mechanism, not both")
	}

	notifying := []ir.Resource{
		{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}},
	}

	switch {
	case caps.NativeNotify:
		playbook := emit(t, notifying...)[PlaybookPath]
		if !strings.Contains(playbook, "notify:") || !strings.Contains(playbook, "handlers:") {
			t.Fatalf("NativeNotify is declared but no native mechanism was emitted:\n%s", playbook)
		}
	case caps.SyntheticNotify:
		t.Skip("this target synthesises notify; see the shared notify contract suite")
	default:
		if err := emitError(t, notifying...); err == nil {
			t.Fatal("a target with no notify support must reject a notifying document")
		}
	}

	runtimeConditional := ir.Resource{ID: "cmd", Type: "exec",
		Params:      map[string]any{"command": "/usr/bin/true"},
		RuntimeWhen: "ansible_facts['os_family'] == 'Debian'"}
	if caps.RuntimeCondition {
		playbook := emit(t, runtimeConditional)[PlaybookPath]
		if !strings.Contains(playbook, "when: ansible_facts['os_family'] == 'Debian'") {
			t.Fatalf("RuntimeCondition is declared but runtimeWhen was not emitted:\n%s", playbook)
		}
	} else if err := emitError(t, runtimeConditional); err == nil {
		t.Fatal("a target without RuntimeCondition must reject runtimeWhen")
	}
}

// TestCompileTimeConditionsNeverReachTheEmitter holds even though this target
// could have evaluated a condition at apply time: `when` is Meridian's to
// evaluate, and output is static by construction.
func TestCompileTimeConditionsNeverReachTheEmitter(t *testing.T) {
	_, err := tryBuildAST(ir.Resource{ID: "pkg", Type: "package",
		Params: map[string]any{"name": "nginx"}, When: "data.install_nginx"})
	if err == nil {
		t.Fatal("a resource carrying an unevaluated `when` must not compile")
	}
	if !strings.Contains(err.Error(), "condition stage") {
		t.Fatalf("error should point at the missing stage: %v", err)
	}

	// The AST type has nowhere to put a compile-time condition, which is the
	// structural half of the same guarantee: an unconditional resource cannot
	// acquire one on the way out.
	if got := emit(t, ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}}); strings.Contains(got[PlaybookPath], "when:") {
		t.Fatalf("an unconditional resource emitted a conditional:\n%s", got[PlaybookPath])
	}
}

func TestEmitterIdentity(t *testing.T) {
	e := New()
	if e.Name() != "ansible" {
		t.Fatalf("name = %q", e.Name())
	}
	types := e.SupportedResourceTypes()
	if !sort.StringsAreSorted(types) {
		t.Fatalf("supported types are not sorted: %v", types)
	}
	if len(types) == 0 {
		t.Fatal("a target that supports no resource type is not a target")
	}
	var _ sdk.Emitter = e
}
