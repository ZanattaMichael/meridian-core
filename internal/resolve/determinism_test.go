package resolve

import (
	"fmt"
	"sync"
	"testing"
)

// determinismFixture exercises all three merge strategies at once, with enough
// keys that a map-iteration-order dependency anywhere in the merge would show
// up as a differing canonical form.
func determinismFixture() (*Hierarchy, Scope, MapLoader) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{
		"nginx":    {Strategy: DeepMerge},
		"packages": {Strategy: UniqueArrayMerge},
		"users":    {Strategy: UniqueArrayMerge, DedupKey: "name"},
	}
	loader := MapLoader{
		"common.yaml": {
			"ntp_server": "pool.ntp.org",
			"packages":   []any{"curl", "vim", "git"},
			"users": []any{
				map[string]any{"name": "deploy", "uid": 1000},
				map[string]any{"name": "ops", "uid": 1001},
			},
			"nginx": map[string]any{
				"workers": 2,
				"limits":  map[string]any{"nofile": 1024, "nproc": 512, "stack": 8192},
			},
		},
		"environment/prod.yaml": {
			"ntp_server": "prod.ntp.internal",
			"packages":   []any{"htop", "vim"},
			"nginx":      map[string]any{"limits": map[string]any{"nofile": 4096}},
		},
		"role/webserver.yaml": {
			"packages": []any{"nginx", "curl"},
			"nginx":    map[string]any{"workers": 4, "listen": 80},
		},
		"node/web01.yaml": {
			"ntp_server": "web01.ntp.internal",
			"users":      []any{map[string]any{"name": "app", "uid": 2000}},
			"nginx":      map[string]any{"limits": map[string]any{"nofile": 65535}},
		},
	}
	return h, standardScope(), loader
}

func resolveCanonical(t *testing.T) string {
	t.Helper()
	h, scope, loader := determinismFixture()
	res, err := Resolve(h, scope, loader)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	return res.Data.Canonical()
}

// TestResolveIsDeterministic treats determinism as a property: the same inputs
// must produce the same output every time, not merely on a lucky run.
func TestResolveIsDeterministic(t *testing.T) {
	want := resolveCanonical(t)
	for i := 0; i < 100; i++ {
		if got := resolveCanonical(t); got != want {
			t.Fatalf("run %d differs:\n got %s\nwant %s", i, got, want)
		}
	}
}

// TestResolveIsDeterministicUnderConcurrency runs resolutions in parallel so the
// Go runtime interleaves them differently on each execution. Map iteration
// order is randomised per range statement, so this widens the net the
// sequential test casts.
func TestResolveIsDeterministicUnderConcurrency(t *testing.T) {
	want := resolveCanonical(t)

	const workers = 16
	results := make([]string, workers)
	var wg sync.WaitGroup
	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			h, scope, loader := determinismFixture()
			res, err := Resolve(h, scope, loader)
			if err != nil {
				results[i] = "error: " + err.Error()
				return
			}
			results[i] = res.Data.Canonical()
		}(i)
	}
	wg.Wait()

	for i, got := range results {
		if got != want {
			t.Fatalf("worker %d differs:\n got %s\nwant %s", i, got, want)
		}
	}
}

// TestResolveIsDeterministicAcrossSubtests uses parallel subtests, which the
// testing package schedules independently of declaration order.
func TestResolveIsDeterministicAcrossSubtests(t *testing.T) {
	want := resolveCanonical(t)
	for i := 0; i < 32; i++ {
		t.Run(fmt.Sprintf("run-%02d", i), func(t *testing.T) {
			t.Parallel()
			if got := resolveCanonical(t); got != want {
				t.Fatalf("differs:\n got %s\nwant %s", got, want)
			}
		})
	}
}

func TestCanonicalIsStableAcrossEquivalentMaps(t *testing.T) {
	a := Data{"b": 2, "a": map[string]any{"y": 2, "x": 1}, "c": []any{3, 1, 2}}
	b := Data{"a": map[string]any{"x": 1, "y": 2}, "c": []any{3, 1, 2}, "b": 2}
	if a.Canonical() != b.Canonical() {
		t.Fatalf("canonical forms differ:\n%s\n%s", a.Canonical(), b.Canonical())
	}
	// Arrays are ordered data: reordering them must change the canonical form.
	c := Data{"a": map[string]any{"x": 1, "y": 2}, "c": []any{1, 2, 3}, "b": 2}
	if a.Canonical() == c.Canonical() {
		t.Fatal("canonical form ignored array order")
	}
}

func TestCanonicalNormalizesUntypedMapKeys(t *testing.T) {
	typed := Data{"a": map[string]any{"x": 1}}
	untyped := Data{"a": map[any]any{"x": 1}}
	if typed.Canonical() != untyped.Canonical() {
		t.Fatalf("canonical forms differ:\n%s\n%s", typed.Canonical(), untyped.Canonical())
	}
}

func TestResolveLayerProvenanceIsOrderedAndSorted(t *testing.T) {
	h, scope, loader := determinismFixture()
	res, err := Resolve(h, scope, loader)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	want := []string{"common.yaml", "environment/prod.yaml", "role/webserver.yaml", "node/web01.yaml"}
	if len(res.Layers) != len(want) {
		t.Fatalf("got %d layers, want %d", len(res.Layers), len(want))
	}
	for i, l := range res.Layers {
		if l.Path != want[i] {
			t.Errorf("layers[%d].Path = %q, want %q", i, l.Path, want[i])
		}
		for j := 1; j < len(l.Keys); j++ {
			if l.Keys[j-1] > l.Keys[j] {
				t.Errorf("layers[%d].Keys not sorted: %v", i, l.Keys)
				break
			}
		}
	}
}
