package plugin

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// builtin is a compiled-in emitter, the other half of what a registry holds.
type builtin struct{ name string }

func (b builtin) Name() string                     { return b.name }
func (b builtin) SupportedResourceTypes() []string { return []string{"package"} }
func (b builtin) Capabilities() sdk.Capabilities   { return sdk.Capabilities{NativeNotify: true} }
func (b builtin) Emit(*sdk.ResourceGraph, sdk.Data) (sdk.Artifact, []sdk.Warning, error) {
	return sdk.Artifact{Files: map[string]string{"out.txt": b.name}}, nil, nil
}

func TestRegisterMakesAnEmitterResolvable(t *testing.T) {
	r := NewRegistry()
	if err := r.Register(builtin{name: "puppet"}); err != nil {
		t.Fatalf("Register: %v", err)
	}

	e, ok := r.Emitter("puppet")
	if !ok {
		t.Fatal("the registered target did not resolve")
	}
	if e.Name() != "puppet" {
		t.Fatalf("emitter name = %q", e.Name())
	}
	info, ok := r.Info("puppet")
	if !ok {
		t.Fatal("no info for the registered target")
	}
	if info.Path != "" {
		t.Fatalf("a compiled-in emitter has no binary, got path %q", info.Path)
	}
	if info.ProtocolVersion != sdk.ProtocolVersion || info.SDKVersion != sdk.Version {
		t.Fatalf("info = %+v", info)
	}
}

func TestUnknownTargetsDoNotResolve(t *testing.T) {
	r := NewRegistry()
	if _, ok := r.Emitter("chef"); ok {
		t.Fatal("an unregistered target resolved")
	}
	if _, ok := r.Info("chef"); ok {
		t.Fatal("an unregistered target reported info")
	}
}

func TestRegisterRefusesAnEmitterItCannotName(t *testing.T) {
	r := NewRegistry()
	if err := r.Register(nil); err == nil {
		t.Fatal("a nil emitter was registered")
	}
	if err := r.Register(builtin{name: ""}); err == nil {
		t.Fatal("an emitter with no target name was registered")
	}
}

// TestADuplicateTargetNameIsRefused is the property that keeps output
// reproducible: if two emitters claimed one name, which one compiled a document
// would depend on load order.
func TestADuplicateTargetNameIsRefused(t *testing.T) {
	r := NewRegistry()
	if err := r.Register(builtin{name: "ansible"}); err != nil {
		t.Fatalf("Register: %v", err)
	}
	err := r.Register(builtin{name: "ansible"})
	if err == nil {
		t.Fatal("a second emitter claimed a name already taken")
	}
	if !containsAll(err.Error(), `"ansible"`, "already registered") {
		t.Fatalf("error = %v", err)
	}
	// The first registration still stands: a refused duplicate is not an
	// overwrite that happened to report itself.
	if e, _ := r.Emitter("ansible"); e == nil {
		t.Fatal("the original registration was lost")
	}
}

func TestAPluginCannotTakeACompiledInTargetsName(t *testing.T) {
	r := NewRegistry()
	t.Cleanup(func() { _ = r.Close() })
	if err := r.Register(builtin{name: "fake"}); err != nil {
		t.Fatalf("Register: %v", err)
	}

	_, err := r.Load(context.Background(), testBinary(), helperOptions("serve")...)
	if err == nil {
		t.Fatal("a plugin claimed a name a compiled-in emitter already had")
	}
	if !containsAll(err.Error(), `"fake"`, "compiled into this binary") {
		t.Fatalf("error should name both claimants: %v", err)
	}
	// The rejected plugin is not left running behind the error.
	if names := r.Names(); len(names) != 1 || names[0] != "fake" {
		t.Fatalf("names = %v", names)
	}
}

func TestLoadRegistersWhatThePluginReported(t *testing.T) {
	r := NewRegistry()
	t.Cleanup(func() { _ = r.Close() })

	info, err := r.Load(context.Background(), testBinary(), helperOptions("serve")...)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if info.Name != "fake" || info.Path != testBinary() {
		t.Fatalf("info = %+v", info)
	}

	e, ok := r.Emitter("fake")
	if !ok {
		t.Fatal("the loaded plugin did not resolve")
	}
	// The registry hands back an sdk.Emitter, so a caller cannot tell a
	// subprocess target from a compiled-in one. That is the whole point.
	art, _, err := e.Emit(sampleGraph(), nil)
	if err != nil {
		t.Fatalf("Emit through the registry: %v", err)
	}
	if art.Files["tree.txt"] != sampleGraph().Canonical() {
		t.Fatalf("the plugin compiled a different tree:\n%s", art.Files["tree.txt"])
	}
}

func TestNamesAreSorted(t *testing.T) {
	r := NewRegistry()
	for _, name := range []string{"terraform", "ansible", "puppet"} {
		if err := r.Register(builtin{name: name}); err != nil {
			t.Fatalf("Register(%s): %v", name, err)
		}
	}
	got := strings.Join(r.Names(), ",")
	if got != "ansible,puppet,terraform" {
		t.Fatalf("names = %s", got)
	}
}

func TestDiscoverLoadsEveryBinaryFollowingTheConvention(t *testing.T) {
	dir := t.TempDir()
	for _, target := range []string{"alpha", "beta"} {
		link(t, dir, BinaryName(target))
	}
	// Neither of these is launched: discovery goes by name, so an unrelated
	// file in the directory is never executed just to find out what it is.
	link(t, dir, "meridian-helper")
	if err := os.WriteFile(filepath.Join(dir, "README.md"), []byte("notes\n"), 0o644); err != nil {
		t.Fatalf("writing README: %v", err)
	}

	r := NewRegistry()
	t.Cleanup(func() { _ = r.Close() })

	loaded, err := r.Discover(context.Background(), dir, helperOptions("serve")...)
	if err != nil {
		t.Fatalf("Discover: %v", err)
	}
	if len(loaded) != 2 || loaded[0].Name != "alpha" || loaded[1].Name != "beta" {
		t.Fatalf("loaded = %+v", loaded)
	}
	if got := strings.Join(r.Names(), ","); got != "alpha,beta" {
		t.Fatalf("names = %s", got)
	}
}

// TestDiscoverTreatsAMissingDirectoryAsNoPlugins: an operator who installed
// none is the normal case, not a misconfiguration to report.
func TestDiscoverTreatsAMissingDirectoryAsNoPlugins(t *testing.T) {
	r := NewRegistry()
	loaded, err := r.Discover(context.Background(), filepath.Join(t.TempDir(), "absent"))
	if err != nil {
		t.Fatalf("Discover: %v", err)
	}
	if len(loaded) != 0 {
		t.Fatalf("loaded = %+v", loaded)
	}
}

func TestDiscoverReportsWhatItManagedToLoad(t *testing.T) {
	dir := t.TempDir()
	link(t, dir, BinaryName("alpha"))
	// Sorted load order puts this second, so the first plugin is already up
	// when the failure happens and has to be reported alongside it.
	if err := os.WriteFile(filepath.Join(dir, BinaryName("zulu")), []byte("#!/bin/false\n"), 0o755); err != nil {
		t.Fatalf("writing the broken plugin: %v", err)
	}

	r := NewRegistry()
	t.Cleanup(func() { _ = r.Close() })

	loaded, err := r.Discover(context.Background(), dir, helperOptions("serve")...)
	if err == nil {
		t.Fatal("a binary that is not a plugin should fail discovery")
	}
	if len(loaded) != 1 || loaded[0].Name != "alpha" {
		t.Fatalf("discovery should report what it loaded before failing, got %+v", loaded)
	}
}

func TestIsPluginNameAppliesTheConvention(t *testing.T) {
	for name, want := range map[string]bool{
		"meridian-target-ansible":     true,
		"meridian-target-ansible.exe": true,
		"meridian-target-":            false,
		"meridian-target":             false,
		"meridian-helper":             false,
		"ansible":                     false,
		"":                            false,
	} {
		if got := isPluginName(name); got != want {
			t.Errorf("isPluginName(%q) = %v, want %v", name, got, want)
		}
	}
}

func TestBinaryNameMatchesTheHostPlatform(t *testing.T) {
	got := BinaryName("ansible")
	want := "meridian-target-ansible"
	if runtime.GOOS == "windows" {
		want += ".exe"
	}
	if got != want {
		t.Fatalf("BinaryName = %q, want %q", got, want)
	}
}

// TestCloseShutsEveryPluginDown: a registry that leaked a process on shutdown
// would leave orphans behind every compile.
func TestCloseShutsEveryPluginDown(t *testing.T) {
	dir := t.TempDir()
	for _, target := range []string{"alpha", "beta"} {
		link(t, dir, BinaryName(target))
	}

	r := NewRegistry()
	if _, err := r.Discover(context.Background(), dir, helperOptions("serve")...); err != nil {
		t.Fatalf("Discover: %v", err)
	}
	if err := r.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if names := r.Names(); len(names) != 0 {
		t.Fatalf("targets outlived the registry: %v", names)
	}
	if err := r.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
}

// helperOptions launches the test binary as a plugin. The target name comes
// from the file name, so one helper can stand in for several plugins at once.
func helperOptions(mode string) []StartOption {
	return []StartOption{
		WithArgs("-test.run=TestPluginHelperProcess"),
		WithEnv(helperEnv + "=" + mode),
	}
}

// link puts the test binary in a directory under a plugin's name.
func link(t *testing.T, dir, name string) {
	t.Helper()
	if err := os.Symlink(testBinary(), filepath.Join(dir, name)); err != nil {
		t.Fatalf("linking %s: %v", name, err)
	}
}
