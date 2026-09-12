// Package contract holds Meridian's tier 3 test: the same document compiled
// twice, once by the emitter linked into this binary and once by the real
// plugin binary in its own process, asserting the two agree byte for byte.
//
// The tests either side of this one each prove half of it. The ansible
// package's own tests prove the emitter is correct; the plugin package's tests
// prove the boundary carries a tree faithfully using a fake emitter. Only this
// one proves the combination: that shipping a target as a subprocess does not
// change a single byte of what an operator applies.
//
// It builds the plugin binary, which is why it is a separate package guarded by
// -short rather than part of the fast unit tests.
package contract

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ast"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/internal/plugin"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
	"github.com/ZanattaMichael/meridian-core/plugins/ansible"
)

const pluginPackage = "github.com/ZanattaMichael/meridian-core/plugins/ansible/cmd/meridian-target-ansible"

// goldenDir is where the emitter's own tests keep the record of its output. The
// contract test reads the same files rather than a copy of them: two records of
// the same truth would eventually disagree.
var goldenDir = filepath.Join("..", "..", "..", "plugins", "ansible", "testdata")

// fixtures mirror the emitter's golden fixtures, because the point of this test
// is that the boundary changes nothing about them.
var fixtures = map[string][]ir.Resource{
	"web_stack": {
		{ID: "nginx_pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
		{ID: "nginx_conf", Type: "file",
			Params: map[string]any{
				"path":    "/etc/nginx/nginx.conf",
				"content": "worker_processes 4;\n",
				"owner":   "root",
				"mode":    "0644",
			},
			DependsOn: []ir.Dependency{{Resource: "nginx_pkg"}},
			Notifies:  []ir.Notification{{Resource: "nginx_svc", Action: "restart"}}},
		{ID: "nginx_site", Type: "file",
			Params:    map[string]any{"path": "/etc/nginx/sites-enabled/app", "content": "server {}\n"},
			DependsOn: []ir.Dependency{{Resource: "nginx_pkg"}},
			Notifies:  []ir.Notification{{Resource: "nginx_svc", Action: "reload"}}},
		{ID: "nginx_svc", Type: "service", State: "running",
			Params:    map[string]any{"name": "nginx", "enabled": true},
			DependsOn: []ir.Dependency{{Resource: "nginx_pkg"}}},
	},
	"accounts": {
		{ID: "deploy_group", Type: "group", Params: map[string]any{"name": "deploy", "gid": 1200}},
		{ID: "deploy_user", Type: "user",
			Params: map[string]any{
				"name":   "deploy",
				"uid":    1200,
				"shell":  "/bin/bash",
				"groups": []any{"deploy", "sudo"},
			},
			DependsOn: []ir.Dependency{{Resource: "deploy_group"}}},
		{ID: "legacy_user", Type: "user", State: "absent", Params: map[string]any{"name": "old"}},
		{ID: "home_dir", Type: "file", State: "directory",
			Params:    map[string]any{"path": "/srv/app", "owner": "deploy", "group": "deploy", "mode": "0750"},
			DependsOn: []ir.Dependency{{Resource: "deploy_user"}}},
	},
	"runtime_condition": {
		{ID: "stale_conf", Type: "file", State: "absent", Params: map[string]any{"path": "/etc/old-app.conf"}},
		{ID: "warm_cache", Type: "exec",
			Params:      map[string]any{"command": "/usr/local/bin/warm-cache --all", "chdir": "/srv/app"},
			RuntimeWhen: "ansible_facts['os_family'] == 'Debian'",
			DependsOn:   []ir.Dependency{{Resource: "stale_conf"}}},
		{ID: "reindex", Type: "exec",
			Params:    map[string]any{"command": "a | b", "shell": true},
			DependsOn: []ir.Dependency{{Resource: "warm_cache"}}},
	},
}

// TestTheRealPluginMatchesTheCompiledInEmitter is the milestone's claim, stated
// as a test: extracting the emitter across the SDK changed nothing an operator
// can observe.
func TestTheRealPluginMatchesTheCompiledInEmitter(t *testing.T) {
	client := loadPlugin(t)

	for name, resources := range fixtures {
		t.Run(name, func(t *testing.T) {
			g := build(t, resources)

			direct, directWarnings, err := ansible.New().Emit(g, nil)
			if err != nil {
				t.Fatalf("compiling in process: %v", err)
			}
			viaPlugin, pluginWarnings, err := client.Emit(g, nil)
			if err != nil {
				t.Fatalf("compiling through the plugin: %v", err)
			}

			if !reflect.DeepEqual(direct.Paths(), viaPlugin.Paths()) {
				t.Fatalf("artifact paths differ: %v vs %v", direct.Paths(), viaPlugin.Paths())
			}
			for _, path := range direct.Paths() {
				if direct.Files[path] != viaPlugin.Files[path] {
					t.Fatalf("%s differs across the boundary:\n--- in process ---\n%s\n--- via plugin ---\n%s",
						path, direct.Files[path], viaPlugin.Files[path])
				}
				// And both match what the emitter's own tests recorded, so a
				// change that broke both identically still fails here.
				golden := filepath.Join(goldenDir, name+"."+strings.ReplaceAll(path, ".", "_"))
				want, err := os.ReadFile(golden)
				if err != nil {
					t.Fatalf("reading %s: %v", golden, err)
				}
				if viaPlugin.Files[path] != string(want) {
					t.Fatalf("%s does not match %s:\n--- via plugin ---\n%s\n--- golden ---\n%s",
						path, golden, viaPlugin.Files[path], want)
				}
			}

			if !reflect.DeepEqual(directWarnings, pluginWarnings) {
				t.Fatalf("warnings differ: %v vs %v", directWarnings, pluginWarnings)
			}
		})
	}
}

// TestThePluginDescribesTheSameTargetItLinks guards the wiring: a plugin binary
// that served something other than the package it imports would pass every
// emitter test and still be wrong.
func TestThePluginDescribesTheSameTargetItLinks(t *testing.T) {
	client := loadPlugin(t)
	e := ansible.New()

	if client.Name() != e.Name() {
		t.Fatalf("name = %q, want %q", client.Name(), e.Name())
	}
	if !reflect.DeepEqual(client.SupportedResourceTypes(), e.SupportedResourceTypes()) {
		t.Fatalf("resource types = %v, want %v", client.SupportedResourceTypes(), e.SupportedResourceTypes())
	}
	if client.Capabilities() != e.Capabilities() {
		t.Fatalf("capabilities = %+v, want %+v", client.Capabilities(), e.Capabilities())
	}
	if info := client.Info(); info.ProtocolVersion != sdk.ProtocolVersion || info.SDKVersion != sdk.Version {
		t.Fatalf("info = %+v", info)
	}
}

// TestARefusalKeepsItsPositionAcrossTheBoundary: a compile error is only useful
// if it still points at the line the author wrote, so the position has to
// survive the wire along with the rule that rejected it.
func TestARefusalKeepsItsPositionAcrossTheBoundary(t *testing.T) {
	client := loadPlugin(t)
	g := build(t, []ir.Resource{
		{ID: "mystery", Type: "firewall_rule", Params: map[string]any{"port": 443},
			Pos: ir.Position{File: "site.yaml", Line: 12, Column: 5}},
	})

	directErr := refusal(t, func() error {
		_, _, err := ansible.New().Emit(g, nil)
		return err
	})
	pluginErr := refusal(t, func() error {
		_, _, err := client.Emit(g, nil)
		return err
	})

	if *pluginErr != *directErr {
		t.Fatalf("the refusal changed in transit:\n--- via plugin ---\n%+v\n--- in process ---\n%+v",
			pluginErr, directErr)
	}
	if pluginErr.Error() != directErr.Error() {
		t.Fatalf("rendered differently:\n%s\n%s", pluginErr.Error(), directErr.Error())
	}
	if pluginErr.Pos.File != "site.yaml" || pluginErr.Pos.Line != 12 {
		t.Fatalf("position was lost: %+v", pluginErr.Pos)
	}
}

// TestThePluginIsDeterministicAcrossCalls: one process compiling the same
// document repeatedly must not drift, which a target holding state between
// calls would.
func TestThePluginIsDeterministicAcrossCalls(t *testing.T) {
	client := loadPlugin(t)
	g := build(t, fixtures["web_stack"])

	first, _, err := client.Emit(g, nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	for i := 0; i < 20; i++ {
		got, _, err := client.Emit(g, nil)
		if err != nil {
			t.Fatalf("Emit (call %d): %v", i, err)
		}
		for _, path := range first.Paths() {
			if got.Files[path] != first.Files[path] {
				t.Fatalf("call %d produced a different %s:\n%s", i, path, got.Files[path])
			}
		}
	}
}

// TestConcurrentCallsToOnePluginAgree: the host compiles hosts in parallel, so
// one plugin process serves overlapping requests.
func TestConcurrentCallsToOnePluginAgree(t *testing.T) {
	client := loadPlugin(t)
	g := build(t, fixtures["web_stack"])

	want, _, err := client.Emit(g, nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}

	got := make([]string, 8)
	var wg sync.WaitGroup
	for i := range got {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			art, _, err := client.Emit(g, nil)
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			got[i] = art.Files[ansible.PlaybookPath]
		}(i)
	}
	wg.Wait()

	for i, out := range got {
		if out != want.Files[ansible.PlaybookPath] {
			t.Fatalf("concurrent call %d differed:\n%s", i, out)
		}
	}
}

// loadPlugin builds the real plugin binary once and starts it per test.
func loadPlugin(t *testing.T) *plugin.Client {
	t.Helper()
	if testing.Short() {
		t.Skip("the contract test builds the plugin binary")
	}
	client, err := plugin.Start(context.Background(), buildPlugin(t))
	if err != nil {
		t.Fatalf("loading the plugin: %v", err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return client
}

var (
	buildOnce sync.Once
	buildPath string
	buildErr  error
	buildDir  string
)

// buildPlugin compiles the plugin binary, once per test run. The binary is a
// build artifact of this test, not a checked-in one: a stale copy would make
// the contract test assert about code nobody is shipping.
func buildPlugin(t *testing.T) string {
	t.Helper()
	buildOnce.Do(func() {
		buildDir, buildErr = os.MkdirTemp("", "meridian-contract-")
		if buildErr != nil {
			return
		}
		buildPath = filepath.Join(buildDir, plugin.BinaryName(ansible.Name))
		out, err := exec.Command("go", "build", "-o", buildPath, pluginPackage).CombinedOutput()
		if err != nil {
			buildErr = errors.New(string(out))
		}
	})
	if buildErr != nil {
		t.Fatalf("building %s: %v", pluginPackage, buildErr)
	}
	return buildPath
}

func TestMain(m *testing.M) {
	code := m.Run()
	if buildDir != "" {
		_ = os.RemoveAll(buildDir)
	}
	os.Exit(code)
}

// build compiles resources into the tree a target receives, exactly as the host
// does before handing it over.
func build(t *testing.T, resources []ir.Resource) *sdk.ResourceGraph {
	t.Helper()
	resources = append([]ir.Resource(nil), resources...)
	for i := range resources {
		resources[i].Index = i
	}
	g, err := ast.Build(&ir.ResourceSet{
		APIVersion: ir.APIVersionV1,
		Kind:       ir.KindResourceSet,
		Metadata:   ir.Metadata{Name: "web"},
		Spec:       ir.ResourceSetSpec{Target: ansible.Name, Resources: resources},
	}, "web01.example.com")
	if err != nil {
		t.Fatalf("building the tree: %v", err)
	}
	return g.ToSDK()
}

// refusal requires a compile error and returns it for comparison.
func refusal(t *testing.T, compile func() error) *sdk.CompileError {
	t.Helper()
	err := compile()
	var ce *sdk.CompileError
	if !errors.As(err, &ce) {
		t.Fatalf("error = %#v, want an *sdk.CompileError", err)
	}
	return ce
}
