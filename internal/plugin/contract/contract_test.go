// Package contract holds Meridian's tier 3 test: the same document compiled
// twice, once by the emitter linked into this binary and once by the real
// plugin binary in its own process, asserting the two agree byte for byte.
//
// The tests either side of this one each prove half of it. Each target package's
// own tests prove that emitter is correct; the plugin package's tests prove the
// boundary carries a tree faithfully using a fake emitter. Only this one proves
// the combination: that shipping a target as a subprocess does not change a
// single byte of what an operator applies.
//
// Every test here runs against every target, because a contract one target
// happens to satisfy is not a contract. Adding a target to the table below is
// what subjects it to all of them.
//
// It builds the plugin binaries, which is why it is a separate package guarded
// by -short rather than part of the fast unit tests.
package contract

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"sync"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ast"
	"github.com/ZanattaMichael/meridian-core/internal/fixtures"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/internal/plugin"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
	"github.com/ZanattaMichael/meridian-core/plugins/ansible"
	"github.com/ZanattaMichael/meridian-core/plugins/puppet"
)

// target describes one shipped plugin well enough to hold it to the contract.
type target struct {
	name    string
	emitter sdk.Emitter
	// pkg is the command whose binary the host would load.
	pkg string
	// goldenDir is where the emitter's own tests keep the record of its output.
	// The contract test reads those files rather than a copy of them: two
	// records of the same truth would eventually disagree.
	goldenDir string
	// refused names the fixtures this target cannot compile. Their golden holds
	// the refusal text instead of an artifact, and the refusal is as much a part
	// of the contract as the output is.
	refused map[string]bool
	// unsupportedType is a resource type no target maps, used to compare how a
	// refusal crosses the wire.
	unsupportedType string
}

var targets = []target{
	{
		name:            ansible.Name,
		emitter:         ansible.New(),
		pkg:             "github.com/ZanattaMichael/meridian-core/plugins/ansible/cmd/meridian-target-ansible",
		goldenDir:       filepath.Join("..", "..", "..", "plugins", "ansible", "testdata"),
		unsupportedType: "firewall_rule",
	},
	{
		name:            puppet.Name,
		emitter:         puppet.New(),
		pkg:             "github.com/ZanattaMichael/meridian-core/plugins/puppet/cmd/meridian-target-puppet",
		goldenDir:       filepath.Join("..", "..", "..", "plugins", "puppet", "testdata"),
		refused:         map[string]bool{"web_stack": true, "runtime_condition": true},
		unsupportedType: "firewall_rule",
	},
}

// TestTheRealPluginMatchesTheCompiledInEmitter is the milestone's claim, stated
// as a test: extracting an emitter across the SDK changed nothing an operator
// can observe.
func TestTheRealPluginMatchesTheCompiledInEmitter(t *testing.T) {
	forEachTarget(t, func(t *testing.T, tg target, client *plugin.Client) {
		for _, name := range fixtures.Names() {
			t.Run(name, func(t *testing.T) {
				g := build(t, tg.name, fixtures.All[name])

				direct, directWarnings, directErr := tg.emitter.Emit(g, nil)
				viaPlugin, pluginWarnings, pluginErr := client.Emit(g, nil)

				if tg.refused[name] {
					if directErr == nil || pluginErr == nil {
						t.Fatalf("%s should refuse fixture %q; in process: %v, via plugin: %v",
							tg.name, name, directErr, pluginErr)
					}
					if directErr.Error() != pluginErr.Error() {
						t.Fatalf("the refusal changed in transit:\n--- in process ---\n%s\n--- via plugin ---\n%s",
							directErr, pluginErr)
					}
					checkGolden(t, filepath.Join(tg.goldenDir, name+".refused"), pluginErr.Error()+"\n")
					return
				}

				if directErr != nil || pluginErr != nil {
					t.Fatalf("compiling %q: in process: %v, via plugin: %v", name, directErr, pluginErr)
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
					checkGolden(t, filepath.Join(tg.goldenDir, name+"."+goldenSuffix(path)), viaPlugin.Files[path])
				}

				if !reflect.DeepEqual(directWarnings, pluginWarnings) {
					t.Fatalf("warnings differ: %v vs %v", directWarnings, pluginWarnings)
				}
			})
		}
	})
}

// TestThePluginDescribesTheSameTargetItLinks guards the wiring: a plugin binary
// that served something other than the package it imports would pass every
// emitter test and still be wrong.
func TestThePluginDescribesTheSameTargetItLinks(t *testing.T) {
	forEachTarget(t, func(t *testing.T, tg target, client *plugin.Client) {
		if client.Name() != tg.emitter.Name() {
			t.Fatalf("name = %q, want %q", client.Name(), tg.emitter.Name())
		}
		if !reflect.DeepEqual(client.SupportedResourceTypes(), tg.emitter.SupportedResourceTypes()) {
			t.Fatalf("resource types = %v, want %v",
				client.SupportedResourceTypes(), tg.emitter.SupportedResourceTypes())
		}
		if client.Capabilities() != tg.emitter.Capabilities() {
			t.Fatalf("capabilities = %+v, want %+v", client.Capabilities(), tg.emitter.Capabilities())
		}
		if info := client.Info(); info.ProtocolVersion != sdk.ProtocolVersion || info.SDKVersion != sdk.Version {
			t.Fatalf("info = %+v", info)
		}
	})
}

// TestARefusalKeepsItsPositionAcrossTheBoundary: a compile error is only useful
// if it still points at the line the author wrote, so the position has to
// survive the wire along with the rule that rejected it.
func TestARefusalKeepsItsPositionAcrossTheBoundary(t *testing.T) {
	forEachTarget(t, func(t *testing.T, tg target, client *plugin.Client) {
		g := build(t, tg.name, []ir.Resource{
			{ID: "mystery", Type: tg.unsupportedType, Params: map[string]any{"port": 443},
				Pos: ir.Position{File: "site.yaml", Line: 12, Column: 5}},
		})

		directErr := refusal(t, func() error {
			_, _, err := tg.emitter.Emit(g, nil)
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
		if pluginErr.Target != tg.name {
			t.Fatalf("refusal names target %q, want %q", pluginErr.Target, tg.name)
		}
	})
}

// TestThePluginIsDeterministicAcrossCalls: one process compiling the same
// document repeatedly must not drift, which a target holding state between
// calls would.
func TestThePluginIsDeterministicAcrossCalls(t *testing.T) {
	forEachTarget(t, func(t *testing.T, tg target, client *plugin.Client) {
		g := build(t, tg.name, compilable(tg))

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
	})
}

// TestConcurrentCallsToOnePluginAgree: the host compiles hosts in parallel, so
// one plugin process serves overlapping requests.
func TestConcurrentCallsToOnePluginAgree(t *testing.T) {
	forEachTarget(t, func(t *testing.T, tg target, client *plugin.Client) {
		g := build(t, tg.name, compilable(tg))

		want, _, err := client.Emit(g, nil)
		if err != nil {
			t.Fatalf("Emit: %v", err)
		}
		path := want.Paths()[0]

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
				got[i] = art.Files[path]
			}(i)
		}
		wg.Wait()

		for i, out := range got {
			if out != want.Files[path] {
				t.Fatalf("concurrent call %d differed:\n%s", i, out)
			}
		}
	})
}

// TestDiscoveryLoadsEveryShippedTarget is the registry's claim held against the
// real binaries rather than fakes. Milestone 3 proved discovery with one plugin,
// which cannot distinguish a registry from a variable: two targets in one
// directory is the first version of this test that means anything.
func TestDiscoveryLoadsEveryShippedTarget(t *testing.T) {
	if testing.Short() {
		t.Skip("the contract test builds the plugin binaries")
	}

	dir := t.TempDir()
	for _, tg := range targets {
		src := buildPlugin(t, tg)
		data, err := os.ReadFile(src)
		if err != nil {
			t.Fatalf("reading %s: %v", src, err)
		}
		if err := os.WriteFile(filepath.Join(dir, filepath.Base(src)), data, 0o755); err != nil {
			t.Fatalf("staging %s: %v", src, err)
		}
	}

	r := plugin.NewRegistry()
	t.Cleanup(func() { _ = r.Close() })

	infos, err := r.Discover(context.Background(), dir)
	if err != nil {
		t.Fatalf("Discover: %v", err)
	}
	if len(infos) != len(targets) {
		t.Fatalf("discovered %d plugins, want %d", len(infos), len(targets))
	}

	want := make([]string, 0, len(targets))
	for _, tg := range targets {
		want = append(want, tg.name)
	}
	sort.Strings(want)
	if got := r.Names(); !reflect.DeepEqual(got, want) {
		t.Fatalf("registered names = %v, want %v", got, want)
	}

	// Each name has to resolve to its own target rather than to whichever
	// plugin the directory listing reached last.
	for _, tg := range targets {
		e, ok := r.Emitter(tg.name)
		if !ok {
			t.Fatalf("%s did not resolve after discovery", tg.name)
		}
		if e.Capabilities() != tg.emitter.Capabilities() {
			t.Fatalf("%s resolved to a target with capabilities %+v, want %+v",
				tg.name, e.Capabilities(), tg.emitter.Capabilities())
		}
	}
}

// TestEachTargetRefusesWhatItCannotDo states the no-silent-capability-loss
// principle as a property of the set rather than of one emitter: targets differ
// in what they support, and a document asking for something a target lacks must
// fail rather than compile to output that does less.
func TestEachTargetRefusesWhatItCannotDo(t *testing.T) {
	runtimeConditional := []ir.Resource{
		{ID: "warm", Type: "exec", Params: map[string]any{"command": "/usr/bin/true"},
			RuntimeWhen: "os_family == 'Debian'"},
	}
	forEachTarget(t, func(t *testing.T, tg target, client *plugin.Client) {
		g := build(t, tg.name, runtimeConditional)
		_, _, err := client.Emit(g, nil)
		if tg.emitter.Capabilities().RuntimeCondition {
			if err != nil {
				t.Fatalf("%s declares RuntimeCondition but refused one: %v", tg.name, err)
			}
			return
		}
		if err == nil {
			t.Fatalf("%s does not declare RuntimeCondition but compiled one anyway", tg.name)
		}
		var ce *sdk.CompileError
		if !errors.As(err, &ce) {
			t.Fatalf("refusal is not a compile error: %#v", err)
		}
		if ce.Resource != "warm" {
			t.Fatalf("refusal names %q, want the resource carrying the condition", ce.Resource)
		}
	})
}

// forEachTarget runs one test body against every shipped target, each with its
// own plugin process.
func forEachTarget(t *testing.T, body func(*testing.T, target, *plugin.Client)) {
	t.Helper()
	for _, tg := range targets {
		t.Run(tg.name, func(t *testing.T) {
			body(t, tg, loadPlugin(t, tg))
		})
	}
}

// compilable returns a fixture this target accepts, for the tests that care
// about repeatability rather than about any particular document.
func compilable(tg target) []ir.Resource {
	for _, name := range fixtures.Names() {
		if !tg.refused[name] {
			return fixtures.All[name]
		}
	}
	return nil
}

// goldenSuffix turns an artifact path into the file name the emitter's own
// tests record it under.
func goldenSuffix(path string) string {
	return strings.NewReplacer("/", "_", ".", "_").Replace(path)
}

func checkGolden(t *testing.T, golden, got string) {
	t.Helper()
	want, err := os.ReadFile(golden)
	if err != nil {
		t.Fatalf("reading %s: %v", golden, err)
	}
	if got != string(want) {
		t.Fatalf("output does not match %s:\n--- got ---\n%s\n--- want ---\n%s", golden, got, want)
	}
}

// loadPlugin builds a target's real plugin binary once and starts it per test.
func loadPlugin(t *testing.T, tg target) *plugin.Client {
	t.Helper()
	if testing.Short() {
		t.Skip("the contract test builds the plugin binary")
	}
	client, err := plugin.Start(context.Background(), buildPlugin(t, tg))
	if err != nil {
		t.Fatalf("loading the %s plugin: %v", tg.name, err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return client
}

var (
	buildMu   sync.Mutex
	buildDir  string
	buildPath = map[string]string{}
)

// buildPlugin compiles a plugin binary, once per test run. The binary is a
// build artifact of this test, not a checked-in one: a stale copy would make
// the contract test assert about code nobody is shipping.
func buildPlugin(t *testing.T, tg target) string {
	t.Helper()
	buildMu.Lock()
	defer buildMu.Unlock()

	if path, ok := buildPath[tg.name]; ok {
		return path
	}
	if buildDir == "" {
		dir, err := os.MkdirTemp("", "meridian-contract-")
		if err != nil {
			t.Fatalf("creating a build directory: %v", err)
		}
		buildDir = dir
	}
	path := filepath.Join(buildDir, plugin.BinaryName(tg.name))
	if out, err := exec.Command("go", "build", "-o", path, tg.pkg).CombinedOutput(); err != nil {
		t.Fatalf("building %s: %s", tg.pkg, out)
	}
	buildPath[tg.name] = path
	return path
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
func build(t *testing.T, targetName string, resources []ir.Resource) *sdk.ResourceGraph {
	t.Helper()
	resources = append([]ir.Resource(nil), resources...)
	for i := range resources {
		resources[i].Index = i
	}
	g, err := ast.Build(&ir.ResourceSet{
		APIVersion: ir.APIVersionV1,
		Kind:       ir.KindResourceSet,
		Metadata:   ir.Metadata{Name: fixtures.Name},
		Spec:       ir.ResourceSetSpec{Target: targetName, Resources: resources},
	}, fixtures.Host)
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
