package puppet

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
	caps := New().Capabilities()

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
		got := emit(t, notifying...)[ManifestPath]
		if !strings.Contains(got, "notify => Service['svc']") {
			t.Fatalf("NativeNotify is declared but no native mechanism was emitted:\n%s", got)
		}
		// Puppet's mechanism refreshes the resource itself, so there is no
		// second declaration anywhere for the notification to point at. A
		// handler block would mean the target had synthesised one after all.
		if strings.Count(got, "service { 'svc':") != 1 {
			t.Fatalf("a natively notifying target declares each resource once:\n%s", got)
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
		RuntimeWhen: "$facts['os']['family'] == 'Debian'"}
	if caps.RuntimeCondition {
		got := emit(t, runtimeConditional)[ManifestPath]
		if !strings.Contains(got, "$facts['os']['family'] == 'Debian'") {
			t.Fatalf("RuntimeCondition is declared but runtimeWhen was not emitted:\n%s", got)
		}
	} else if err := emitError(t, runtimeConditional); err == nil {
		t.Fatal("a target without RuntimeCondition must reject runtimeWhen")
	}
}

// TestTheRefusalExplainsTheCapabilityItLacks. Declaring RuntimeCondition false
// costs nothing if the resulting message only says no. An author who wrote
// runtimeWhen needs to know that a catalog is compiled before it reaches the
// node, because that is what tells them the document has to change rather than
// the target.
func TestTheRefusalExplainsTheCapabilityItLacks(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "cmd", Type: "exec",
		Params:      map[string]any{"command": "/usr/bin/true"},
		RuntimeWhen: "$facts['os']['family'] == 'Debian'"})

	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "unsupported-runtime-condition" {
		t.Fatalf("rule = %q", ce.Rule)
	}
	if ce.Resource != "cmd" {
		t.Fatalf("reported against %q, want the resource carrying the condition", ce.Resource)
	}
	if !strings.Contains(ce.Msg, "catalog") {
		t.Fatalf("the refusal should explain why this target cannot: %q", ce.Msg)
	}
}

// TestCompileTimeConditionsNeverReachTheEmitter holds for every target: `when`
// is Meridian's to evaluate, and output is static by construction.
func TestCompileTimeConditionsNeverReachTheEmitter(t *testing.T) {
	_, err := tryBuildAST(ir.Resource{ID: "pkg", Type: "package",
		Params: map[string]any{"name": "nginx"}, When: "data.install_nginx"})
	if err == nil {
		t.Fatal("a resource carrying an unevaluated `when` must not compile")
	}
	if !strings.Contains(err.Error(), "condition stage") {
		t.Fatalf("error should point at the missing stage: %v", err)
	}
}

func TestEmitterIdentity(t *testing.T) {
	e := New()
	if e.Name() != "puppet" {
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

// TestANilTreeIsAnErrorNotAPanic covers the boundary the plugin binary sits on:
// the tree arrives as decoded bytes, so nothing upstream of this method
// guarantees it is there at all.
func TestANilTreeIsAnErrorNotAPanic(t *testing.T) {
	_, _, err := New().Emit(nil, nil)
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "no-input" {
		t.Fatalf("rule = %q, want no-input", ce.Rule)
	}
}
