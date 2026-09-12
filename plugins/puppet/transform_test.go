package puppet

import (
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

func TestEveryMappedTypeProducesADeclaration(t *testing.T) {
	cases := []struct {
		resource   ir.Resource
		wantType   string
		wantEnsure string
		wantAttrs  map[string]any
	}{
		{
			resource:   ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
			wantType:   "package",
			wantEnsure: "present",
			wantAttrs:  map[string]any{"name": "nginx"},
		},
		{
			resource:   ir.Resource{ID: "svc", Type: "service", State: "stopped", Params: map[string]any{"name": "nginx", "enabled": false}},
			wantType:   "service",
			wantEnsure: "stopped",
			wantAttrs:  map[string]any{"name": "nginx", "enable": false},
		},
		{
			resource:   ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf", "content": "x\n"}},
			wantType:   "file",
			wantEnsure: "file",
			wantAttrs:  map[string]any{"path": "/etc/n.conf", "content": "x\n"},
		},
		{
			resource:   ir.Resource{ID: "dir", Type: "file", State: "directory", Params: map[string]any{"path": "/srv"}},
			wantType:   "file",
			wantEnsure: "directory",
			wantAttrs:  map[string]any{"path": "/srv"},
		},
		{
			resource:   ir.Resource{ID: "u", Type: "user", Params: map[string]any{"name": "deploy", "uid": 1200}},
			wantType:   "user",
			wantEnsure: "present",
			wantAttrs:  map[string]any{"name": "deploy", "uid": 1200},
		},
		{
			resource:   ir.Resource{ID: "g", Type: "group", Params: map[string]any{"name": "deploy", "gid": 1200}},
			wantType:   "group",
			wantEnsure: "present",
			wantAttrs:  map[string]any{"name": "deploy", "gid": 1200},
		},
		{
			resource:  ir.Resource{ID: "cmd", Type: "exec", Params: map[string]any{"command": "/bin/true", "chdir": "/srv"}},
			wantType:  "exec",
			wantAttrs: map[string]any{"command": "/bin/true", "cwd": "/srv"},
		},
	}

	for _, c := range cases {
		t.Run(c.resource.Type+"/"+c.resource.ID, func(t *testing.T) {
			r := declared(t, compiled(t, c.resource), c.resource.ID)
			if r.Type != c.wantType {
				t.Fatalf("puppet type = %q, want %q", r.Type, c.wantType)
			}
			if c.wantEnsure == "" {
				if _, ok := r.Attrs["ensure"]; ok {
					t.Fatalf("type %q emitted an ensure it has no state for: %v", c.wantType, r.Attrs["ensure"])
				}
			} else if got := r.Attrs["ensure"]; got != bareword(c.wantEnsure) {
				t.Fatalf("ensure = %v, want %q", got, c.wantEnsure)
			}
			for name, want := range c.wantAttrs {
				if got := r.Attrs[name]; got != want {
					t.Fatalf("attribute %q = %v, want %v", name, got, want)
				}
			}
		})
	}
}

// TestEnsureIsABarewordNotAString is the difference between output a Puppet
// author would have written and output that merely parses. A quoted ensure
// value is accepted by Puppet, so nothing would fail; it would just be wrong in
// a way only a reader notices.
func TestEnsureIsABarewordNotAString(t *testing.T) {
	mf := emit(t, ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}})[ManifestPath]
	if !strings.Contains(mf, "ensure => present,") {
		t.Fatalf("ensure was not written as a bareword:\n%s", mf)
	}
	if strings.Contains(mf, "ensure => 'present'") {
		t.Fatalf("ensure was quoted:\n%s", mf)
	}
}

func TestExecShellSelectsThePuppetShellProvider(t *testing.T) {
	r := declared(t, compiled(t, ir.Resource{ID: "cmd", Type: "exec",
		Params: map[string]any{"command": "a | b", "shell": true}}), "cmd")
	if got := r.Attrs["provider"]; got != bareword("shell") {
		t.Fatalf("provider = %v, want the shell provider", got)
	}
	if _, ok := r.Attrs["shell"]; ok {
		t.Fatal("shell steers the mapping and must not also become an attribute")
	}
}

func TestAnUnsupportedTypeNamesWhatWouldHaveWorked(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "x", Type: "firewall_rule", Params: map[string]any{"name": "x"}})
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a CompileError: %T %v", err, err)
	}
	if ce.Rule != "unsupported-resource-type" {
		t.Fatalf("rule = %q", ce.Rule)
	}
	for _, want := range []string{"firewall_rule", "package", "service", "exec"} {
		if !strings.Contains(ce.Msg, want) {
			t.Fatalf("message does not mention %q: %s", want, ce.Msg)
		}
	}
}

func TestAnUnsupportedParamNamesWhatWouldHaveWorked(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "pkg", Type: "package",
		Params: map[string]any{"name": "nginx", "install_options": "--nodeps"}})
	if !strings.Contains(err.Error(), "install_options") || !strings.Contains(err.Error(), "accepted: name") {
		t.Fatalf("message should name the param and the accepted set: %v", err)
	}
}

// TestRestartedIsNotAStateOnThisTarget records a deliberate difference from the
// Ansible target, which accepts it because a task is an action. A catalog says
// what a node should look like, and "restarted" is not something a node can be.
func TestRestartedIsNotAStateOnThisTarget(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "svc", Type: "service", State: "restarted",
		Params: map[string]any{"name": "nginx"}})
	if !strings.Contains(err.Error(), "does not accept state") {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(err.Error(), "running") {
		t.Fatalf("message should name the states that would have worked: %v", err)
	}
}

func TestAMissingRequiredParamIsRefused(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{}})
	if !strings.Contains(err.Error(), `requires param "name"`) {
		t.Fatalf("unexpected error: %v", err)
	}
}

// TestRuntimeWhenIsRefusedWithAReason is the no-silent-capability-loss principle
// at its most concrete. A catalog has no apply-time conditional, so the only
// honest options are to fail or to emit a manifest that ignores what the author
// asked for.
func TestRuntimeWhenIsRefusedWithAReason(t *testing.T) {
	err := emitError(t, ir.Resource{ID: "cmd", Type: "exec",
		Params: map[string]any{"command": "/bin/true"}, RuntimeWhen: "$facts['os']['family'] == 'Debian'"})
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a CompileError: %T", err)
	}
	if ce.Rule != "unsupported-runtime-condition" {
		t.Fatalf("rule = %q", ce.Rule)
	}
	if !strings.Contains(ce.Msg, "compiled before it reaches the node") {
		t.Fatalf("message should say why, not just that: %s", ce.Msg)
	}
}

func TestNotifyBecomesANativeMetaparameter(t *testing.T) {
	m := compiled(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "restart"}}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}})

	conf := declared(t, m, "conf")
	if len(conf.Notify) != 1 || conf.Notify[0].String() != "Service['svc']" {
		t.Fatalf("notify = %v, want one reference to Service['svc']", conf.Notify)
	}
	// Puppet has no handler block: the notified resource is the declaration
	// itself, not a synthesised copy of it. A second declaration would be a
	// duplicate the agent rejects.
	if len(m.Resources) != 2 {
		t.Fatalf("manifest holds %d resources; notification must not synthesise one", len(m.Resources))
	}
}

func TestTwoEdgesToOneResourceCollapseOntoOneNotify(t *testing.T) {
	m := compiled(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			Notifies: []ir.Notification{
				{Resource: "svc", Action: "restart"},
				{Resource: "svc", Action: "refresh"},
			}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}})
	if got := declared(t, m, "conf").Notify; len(got) != 1 {
		t.Fatalf("notify = %v; a refresh happens once however many times it was triggered", got)
	}
}

func TestNotifyingAnUnrefreshableTypeIsRefused(t *testing.T) {
	err := emitError(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			Notifies: []ir.Notification{{Resource: "pkg", Action: "restart"}}},
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}})
	if !strings.Contains(err.Error(), "cannot refresh") {
		t.Fatalf("unexpected error: %v", err)
	}
}

// TestARefusedActionIsReportedAgainstTheNotifier checks the half of an error
// message that decides whether it is useful: which line the author is sent to.
// The action was written on the notifier, so the notified resource's own
// declaration is not the thing to change.
func TestARefusedActionIsReportedAgainstTheNotifier(t *testing.T) {
	err := emitError(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/n.conf"},
			Notifies: []ir.Notification{{Resource: "svc", Action: "reload"}}},
		ir.Resource{ID: "svc", Type: "service", Params: map[string]any{"name": "nginx"}})

	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a CompileError: %T", err)
	}
	if ce.Resource != "conf" {
		t.Fatalf("error names resource %q; the reload was declared on %q", ce.Resource, "conf")
	}
	if !strings.Contains(ce.Msg, "svc") {
		t.Fatalf("message should still name the resource being notified: %s", ce.Msg)
	}
}

func TestEveryRefusedActionExplainsItself(t *testing.T) {
	for action, why := range mappings["service"].refusedActions {
		t.Run(action, func(t *testing.T) {
			if len(why) < 40 {
				t.Fatalf("the explanation for %q is too short to be an explanation: %q", action, why)
			}
			if mappings["service"].actions[action] {
				t.Fatalf("action %q is both supported and refused", action)
			}
		})
	}
}

func TestADocumentNameThatCannotBecomeAClassIsRefused(t *testing.T) {
	if _, err := className("Web-Stack"); err == nil {
		t.Fatal("a document name with a capital and a dash cannot be a Puppet class name")
	}
	got, err := className("web")
	if err != nil {
		t.Fatalf("className: %v", err)
	}
	if got != "meridian_web" {
		t.Fatalf("class = %q", got)
	}
}

func TestReferencesAreWrittenTheWayPuppetWritesThem(t *testing.T) {
	cases := map[reference]string{
		{Type: "service", Title: "nginx_svc"}: "Service['nginx_svc']",
		{Type: "exec", Title: "warm"}:         "Exec['warm']",
		{Type: "file", Title: "conf"}:         "File['conf']",
	}
	for ref, want := range cases {
		if got := ref.String(); got != want {
			t.Fatalf("%v rendered as %q, want %q", ref, got, want)
		}
	}
}

// asCompileError is errors.As, spelled out so the test files do not need to
// agree with the SDK on which error package they import.
func asCompileError(err error, target **sdk.CompileError) bool {
	for err != nil {
		if ce, ok := err.(*sdk.CompileError); ok {
			*target = ce
			return true
		}
		u, ok := err.(interface{ Unwrap() error })
		if !ok {
			return false
		}
		err = u.Unwrap()
	}
	return false
}
