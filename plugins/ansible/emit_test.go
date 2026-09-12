package ansible

import (
	"flag"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/fixtures"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
	"gopkg.in/yaml.v3"
)

var update = flag.Bool("update", false, "rewrite the golden files from current output")

// The documents these tests compile live in internal/fixtures, shared with
// every other target's tests. The golden files below and the Puppet target's
// are then renderings of the same documents, which is what makes reading them
// side by side a comparison of the targets rather than of two sets of inputs.

func TestGoldenFiles(t *testing.T) {
	for name, resources := range fixtures.All {
		t.Run(name, func(t *testing.T) {
			files := emit(t, resources...)
			for _, path := range []string{PlaybookPath, InventoryPath} {
				golden := filepath.Join("testdata", name+"."+strings.ReplaceAll(path, ".", "_"))
				got := files[path]
				if *update {
					if err := os.WriteFile(golden, []byte(got), 0o644); err != nil {
						t.Fatalf("writing %s: %v", golden, err)
					}
					continue
				}
				want, err := os.ReadFile(golden)
				if err != nil {
					t.Fatalf("reading %s (run go test -update to create it): %v", golden, err)
				}
				if got != string(want) {
					t.Errorf("%s does not match %s.\n--- got ---\n%s\n--- want ---\n%s",
						path, golden, got, want)
				}
			}
		})
	}
}

// TestNoConditionalConstructsLeak is the automated check the testing plan asks
// for: a compile-time `when` must be evaluated and pruned upstream, so the only
// conditional allowed in emitted output is one an author explicitly asked for
// with runtimeWhen.
func TestNoConditionalConstructsLeak(t *testing.T) {
	for name, resources := range fixtures.All {
		t.Run(name, func(t *testing.T) {
			runtimeConditions := 0
			for _, r := range resources {
				if r.RuntimeWhen != "" {
					runtimeConditions++
				}
			}
			playbook := emit(t, resources...)[PlaybookPath]
			if got := strings.Count(playbook, "when:"); got != runtimeConditions {
				t.Fatalf("playbook holds %d conditionals, want %d (one per runtimeWhen):\n%s",
					got, runtimeConditions, playbook)
			}
		})
	}
}

func TestEmittedPlaybookParsesAsYAML(t *testing.T) {
	for name, resources := range fixtures.All {
		t.Run(name, func(t *testing.T) {
			var plays []map[string]any
			if err := yaml.Unmarshal([]byte(emit(t, resources...)[PlaybookPath]), &plays); err != nil {
				t.Fatalf("emitted playbook is not valid YAML: %v", err)
			}
			if len(plays) != 1 {
				t.Fatalf("expected exactly one play, got %d", len(plays))
			}
			if plays[0]["hosts"] != "web01.example.com" {
				t.Fatalf("play hosts = %v", plays[0]["hosts"])
			}
			tasks, ok := plays[0]["tasks"].([]any)
			if !ok || len(tasks) != len(resources) {
				t.Fatalf("expected %d tasks, got %v", len(resources), plays[0]["tasks"])
			}
		})
	}
}

func TestInventoryNamesTheResolvedHost(t *testing.T) {
	got := emit(t, ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}})[InventoryPath]
	if got != "[meridian]\nweb01.example.com\n" {
		t.Fatalf("inventory = %q", got)
	}
}

func TestArtifactPathsAreSorted(t *testing.T) {
	art, _, err := New().Emit(buildAST(t,
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}}), nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	got := art.Paths()
	if len(got) != 2 || got[0] != InventoryPath || got[1] != PlaybookPath {
		t.Fatalf("paths = %v", got)
	}
}

// TestEmitIsPureRegardlessOfHowTheTreeWasBuilt asserts emit holds no state of
// its own: a tree assembled by hand renders exactly like the same tree derived
// from a document.
func TestEmitIsPureRegardlessOfHowTheTreeWasBuilt(t *testing.T) {
	fromDocument := buildAST(t, fixtures.All["web_stack"]...)

	// Assembled field by field rather than derived, which is exactly what a
	// tree arriving over the plugin boundary is: plain data, with no memory of
	// how it was built.
	byHand := &sdk.ResourceGraph{
		Name:      fromDocument.Name,
		Host:      fromDocument.Host,
		Target:    fromDocument.Target,
		Resources: append([]sdk.Resource(nil), fromDocument.Resources...),
		Edges:     append([]sdk.Edge(nil), fromDocument.Edges...),
	}

	a, _, err := New().Emit(fromDocument, nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	b, _, err := New().Emit(byHand, nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	for _, path := range a.Paths() {
		if a.Files[path] != b.Files[path] {
			t.Fatalf("%s differs between an equal pair of trees:\n%s\n---\n%s",
				path, a.Files[path], b.Files[path])
		}
	}
}

func TestEmitIsByteIdenticalAcrossRuns(t *testing.T) {
	want := emit(t, fixtures.All["web_stack"]...)[PlaybookPath]
	for i := 0; i < 100; i++ {
		if got := emit(t, fixtures.All["web_stack"]...)[PlaybookPath]; got != want {
			t.Fatalf("run %d differed:\n%s", i, got)
		}
	}
}

func TestEmitIsByteIdenticalUnderConcurrency(t *testing.T) {
	want := emit(t, fixtures.All["web_stack"]...)[PlaybookPath]
	got := make([]string, 16)
	var wg sync.WaitGroup
	for i := range got {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			g, err := tryBuildAST(fixtures.All["web_stack"]...)
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			art, _, err := New().Emit(g, nil)
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			got[i] = art.Files[PlaybookPath]
		}(i)
	}
	wg.Wait()
	for i, g := range got {
		if g != want {
			t.Fatalf("goroutine %d differed:\n%s", i, g)
		}
	}
}
