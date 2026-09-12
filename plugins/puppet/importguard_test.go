package puppet

import (
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestTheEmitterDependsOnTheSDKAlone is what makes this target evidence rather
// than merely a second emitter.
//
// Milestone 3 claimed the SDK was a complete contract, but it proved it against
// an emitter written before the SDK existed and moved onto it afterwards. This
// one was written against the SDK from the first line, so the guard is the
// difference between "the SDK was enough for the emitter we already had" and
// "the SDK is enough for an emitter it never saw". If anything here had needed
// internal/, the gap would be in the SDK and no third party could write a
// plugin either.
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
	path := filepath.Join("cmd", "meridian-target-puppet", "main.go")
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
