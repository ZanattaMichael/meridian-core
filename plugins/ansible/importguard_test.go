package ansible

import (
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestTheEmitterDependsOnTheSDKAlone is the structural claim milestone 3 makes.
//
// A third-party target cannot import internal/, so if this emitter needed
// anything from there the SDK would be incomplete and nobody outside this
// repository could write a plugin. Asserting it in a test rather than trusting
// review means the gap shows up the first time someone reaches for an internal
// helper, not after the SDK has been published.
//
// Test files are exempt: they build fixtures through the real IR and AST on
// purpose, because a plugin's tests run inside this module while the plugin
// itself must not depend on it.
func TestTheEmitterDependsOnTheSDKAlone(t *testing.T) {
	const internalPrefix = `"github.com/ZanattaMichael/meridian-core/internal/`

	entries, err := os.ReadDir(".")
	if err != nil {
		t.Fatalf("reading the package directory: %v", err)
	}

	checked := 0
	for _, e := range entries {
		name := e.Name()
		if e.IsDir() || !strings.HasSuffix(name, ".go") || strings.HasSuffix(name, "_test.go") {
			continue
		}
		checked++

		file, err := parser.ParseFile(token.NewFileSet(), name, nil, parser.ImportsOnly)
		if err != nil {
			t.Fatalf("parsing %s: %v", name, err)
		}
		for _, imp := range file.Imports {
			if strings.HasPrefix(imp.Path.Value, internalPrefix) {
				t.Errorf("%s imports %s; a plugin may only depend on pkg/sdk, "+
					"so anything it needs from there belongs in the SDK", name, imp.Path.Value)
			}
		}
	}

	if checked == 0 {
		t.Fatal("found no source files to check; the guard would pass vacuously")
	}
}

// TestThePluginBinaryIsOnlyWiring guards the other half of the split: the
// command exists to serve this package, and logic that drifts into it would be
// logic no in-process test covers.
func TestThePluginBinaryIsOnlyWiring(t *testing.T) {
	path := filepath.Join("cmd", "meridian-target-ansible", "main.go")
	src, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading %s: %v", path, err)
	}
	const maxLines = 40
	if lines := strings.Count(string(src), "\n"); lines > maxLines {
		t.Fatalf("%s is %d lines; a plugin binary over %d lines is holding decisions "+
			"that belong in the emitter package", path, lines, maxLines)
	}
}
