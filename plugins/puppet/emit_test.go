package puppet

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
)

var update = flag.Bool("update", false, "rewrite the golden files from current output")

// The documents these tests compile live in internal/fixtures, shared with the
// Ansible target's tests. That sharing is the point: where a golden file here
// differs from the one next to it, the difference is the target's, because the
// input was identical.
//
// Two of the four fixtures do not compile on this target at all, and their
// golden file is the refusal rather than an artifact. That is not a gap in the
// emitter. A document asking for a reload, or for a condition evaluated on the
// node, is asking for something a Puppet catalog cannot do, and the refusal is
// the target behaving correctly. Recording it as a golden means the wording an
// operator will read is reviewed like any other output, and that a future change
// which quietly starts compiling one of these has to say so in a diff.

// refused lists the fixtures this target cannot compile, with a note on why, so
// a reader of this file does not have to open the golden to find out.
var refused = map[string]string{
	"web_stack":         "notifies a reload, and Puppet's refresh only restarts",
	"runtime_condition": "carries runtimeWhen, and a catalog is compiled before it reaches the node",
}

func TestGoldenFiles(t *testing.T) {
	for _, name := range fixtures.Names() {
		t.Run(name, func(t *testing.T) {
			art, _, err := New().Emit(buildAST(t, fixtures.All[name]...), nil)

			if _, wantRefusal := refused[name]; wantRefusal {
				if err == nil {
					t.Fatalf("fixture %q compiled, but this target should refuse it: %s", name, refused[name])
				}
				checkGolden(t, filepath.Join("testdata", name+".refused"), err.Error()+"\n")
				return
			}
			if err != nil {
				t.Fatalf("Emit: %v", err)
			}
			for _, path := range []string{ManifestPath, SitePath} {
				golden := filepath.Join("testdata", name+"."+goldenSuffix(path))
				checkGolden(t, golden, art.Files[path])
			}
		})
	}
}

// goldenSuffix turns an artifact path into a file name that survives being
// listed next to the Ansible target's, which uses the same scheme.
func goldenSuffix(path string) string {
	return strings.NewReplacer("/", "_", ".", "_").Replace(path)
}

func checkGolden(t *testing.T, golden, got string) {
	t.Helper()
	if *update {
		if err := os.MkdirAll(filepath.Dir(golden), 0o755); err != nil {
			t.Fatalf("creating %s: %v", filepath.Dir(golden), err)
		}
		if err := os.WriteFile(golden, []byte(got), 0o644); err != nil {
			t.Fatalf("writing %s: %v", golden, err)
		}
		return
	}
	want, err := os.ReadFile(golden)
	if err != nil {
		t.Fatalf("reading %s (run go test -update to create it): %v", golden, err)
	}
	if got != string(want) {
		t.Errorf("output does not match %s.\n--- got ---\n%s\n--- want ---\n%s", golden, got, want)
	}
}

// TestNoConditionalConstructsLeak is the automated check the testing plan asks
// for. On this target it is almost trivially satisfied, because a document
// carrying any condition at all is refused; the test is here anyway so that the
// day this target grows a conditional construct, it has to be declared.
func TestNoConditionalConstructsLeak(t *testing.T) {
	for _, name := range fixtures.Names() {
		if _, skip := refused[name]; skip {
			continue
		}
		t.Run(name, func(t *testing.T) {
			mf := emit(t, fixtures.All[name]...)[ManifestPath]
			for _, construct := range []string{"if ", "unless ", "case ", " ? "} {
				if strings.Contains(mf, construct) {
					t.Fatalf("manifest holds the conditional construct %q:\n%s", construct, mf)
				}
			}
		})
	}
}

// TestEveryResourceReachesTheManifest is the structural counterpart to the
// golden files: a target that dropped a resource would still produce output
// that looks plausible, and only a count catches it.
func TestEveryResourceReachesTheManifest(t *testing.T) {
	for _, name := range fixtures.Names() {
		if _, skip := refused[name]; skip {
			continue
		}
		t.Run(name, func(t *testing.T) {
			mf := emit(t, fixtures.All[name]...)[ManifestPath]
			for _, r := range fixtures.All[name] {
				if !strings.Contains(mf, "'"+r.ID+"':") {
					t.Fatalf("resource %q does not appear in the manifest:\n%s", r.ID, mf)
				}
			}
		})
	}
}

func TestSiteAssignsTheClassToTheResolvedHost(t *testing.T) {
	got := emit(t, ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}})[SitePath]
	if !strings.Contains(got, "node 'web01.example.com' {") {
		t.Fatalf("site manifest does not name the resolved host:\n%s", got)
	}
	if !strings.Contains(got, "include meridian_web") {
		t.Fatalf("site manifest does not include the compiled class:\n%s", got)
	}
}

func TestBothArtifactFilesAreMarkedGenerated(t *testing.T) {
	files := emit(t, ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}})
	for path, content := range files {
		if !strings.HasPrefix(content, "# Generated by Meridian.") {
			t.Fatalf("%s is not marked as generated:\n%s", path, content)
		}
	}
}

func TestArtifactPathsAreSorted(t *testing.T) {
	art, _, err := New().Emit(buildAST(t,
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}}), nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	got := art.Paths()
	if len(got) != 2 || got[0] != ManifestPath || got[1] != SitePath {
		t.Fatalf("paths = %v", got)
	}
}

// TestEmitIsPureRegardlessOfHowTheTreeWasBuilt asserts emit holds no state of
// its own: a tree assembled by hand renders exactly like the same tree derived
// from a document.
func TestEmitIsPureRegardlessOfHowTheTreeWasBuilt(t *testing.T) {
	fromDocument := buildAST(t, fixtures.All["restart_chain"]...)

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
	want := emit(t, fixtures.All["restart_chain"]...)[ManifestPath]
	for i := 0; i < 100; i++ {
		if got := emit(t, fixtures.All["restart_chain"]...)[ManifestPath]; got != want {
			t.Fatalf("run %d differed:\n%s", i, got)
		}
	}
}

func TestEmitIsByteIdenticalUnderConcurrency(t *testing.T) {
	want := emit(t, fixtures.All["restart_chain"]...)[ManifestPath]
	got := make([]string, 16)
	var wg sync.WaitGroup
	for i := range got {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			g, err := tryBuildAST(fixtures.All["restart_chain"]...)
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			art, _, err := New().Emit(g, nil)
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			got[i] = art.Files[ManifestPath]
		}(i)
	}
	wg.Wait()
	for i, g := range got {
		if g != want {
			t.Fatalf("goroutine %d differed:\n%s", i, g)
		}
	}
}
