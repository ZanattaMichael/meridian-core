package ansible

import (
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ast"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// These helpers build a tree the way the host does: parse IR, build the AST,
// convert it to the SDK form a plugin receives. A third-party plugin cannot
// import internal/, and does not need to — it never builds a tree, it is handed
// one. Reaching for the real builder here rather than hand-assembling trees
// means these tests exercise the same order and the same params a running host
// would produce, which is the property the golden files are asserting.
//
// importguard_test.go is what holds the other half of the line: the emitter's
// own sources depend on pkg/sdk and nothing else.

// buildAST assembles a ResourceSet and compiles it to an AST, so each test can
// start from the IR an author would actually write.
func buildAST(t *testing.T, resources ...ir.Resource) *sdk.ResourceGraph {
	t.Helper()
	g, err := tryBuildAST(resources...)
	if err != nil {
		t.Fatalf("building the AST: %v", err)
	}
	return g
}

func tryBuildAST(resources ...ir.Resource) (*sdk.ResourceGraph, error) {
	// Copy before stamping declaration order: the fixtures are shared across
	// tests, including ones that compile the same fixture concurrently.
	resources = append([]ir.Resource(nil), resources...)
	for i := range resources {
		resources[i].Index = i
	}
	rs := &ir.ResourceSet{
		APIVersion: ir.APIVersionV1,
		Kind:       ir.KindResourceSet,
		Metadata:   ir.Metadata{Name: "web"},
		Spec:       ir.ResourceSetSpec{Target: Name, Resources: resources},
	}
	g, err := ast.Build(rs, "web01.example.com")
	if err != nil {
		return nil, err
	}
	return g.ToSDK(), nil
}

// emit compiles resources all the way to an artifact.
func emit(t *testing.T, resources ...ir.Resource) map[string]string {
	t.Helper()
	art, _, err := New().Emit(buildAST(t, resources...), nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	return art.Files
}

// emitError compiles and requires a failure, returning it for inspection.
func emitError(t *testing.T, resources ...ir.Resource) error {
	t.Helper()
	g, err := tryBuildAST(resources...)
	if err != nil {
		return err
	}
	_, _, err = New().Emit(g, nil)
	if err == nil {
		t.Fatal("expected a compile error, got none")
	}
	return err
}

// taskNamed finds a transformed task by name.
func taskNamed(t *testing.T, pb *playbook, name string) task {
	t.Helper()
	for _, task := range append(append([]task{}, pb.Tasks...), pb.Handlers...) {
		if task.Name == name {
			return task
		}
	}
	t.Fatalf("no task or handler named %q", name)
	return task{}
}

// transformed runs only the transform stage.
func transformed(t *testing.T, resources ...ir.Resource) *playbook {
	t.Helper()
	pb, err := transform(buildAST(t, resources...))
	if err != nil {
		t.Fatalf("transform: %v", err)
	}
	return pb
}
