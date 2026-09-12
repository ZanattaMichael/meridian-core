package plugin

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// The tests in this package need a real plugin process, and building one with
// the Go toolchain on every run would make the fast tests slow. Instead the
// test binary re-executes itself: TestPluginHelperProcess is an ordinary test
// that does nothing unless the environment says it is being run as a plugin, in
// which case it serves an emitter and exits before the testing framework can
// print anything to the stdout the protocol owns.
//
// The contract test alongside these does build the real binary, because proving
// the real plugin behaves identically is worth the compile.

const (
	helperEnv  = "MERIDIAN_PLUGIN_HELPER"
	helperName = "MERIDIAN_PLUGIN_HELPER_NAME"
)

// TestPluginHelperProcess is not a test. It is the plugin the other tests load.
func TestPluginHelperProcess(t *testing.T) {
	mode := os.Getenv(helperEnv)
	if mode == "" {
		t.Skip("not running as a plugin")
	}

	name := os.Getenv(helperName)
	if name == "" {
		name = nameFromBinary(os.Args[0])
	}

	switch mode {
	case "serve":
		if err := sdk.Serve(helperEmitter{name: name}); err != nil {
			fmt.Fprintln(os.Stderr, "helper plugin:", err)
			os.Exit(1)
		}
		os.Exit(0)
	case "refuse":
		if err := sdk.Serve(helperEmitter{name: name, refuse: true}); err != nil {
			os.Exit(1)
		}
		os.Exit(0)
	case "silent":
		// Announces nothing and waits, so the host's handshake timeout is what
		// ends the test rather than the process. A bounded sleep rather than a
		// blocked select: a deadlocked process would exit, and the host would
		// then report the exit instead of the timeout under test.
		time.Sleep(time.Minute)
	case "noise":
		fmt.Println("hello, I am not a Meridian plugin")
		os.Exit(0)
	case "crash":
		fmt.Fprintln(os.Stderr, "helper plugin: could not read its configuration")
		os.Exit(3)
	case "wrong-protocol":
		fmt.Printf("%s|%d|%s|tcp|127.0.0.1:1\n", sdk.HandshakeMagic, sdk.ProtocolVersion+1, sdk.Version)
		time.Sleep(time.Minute)
	}
	os.Exit(0)
}

// nameFromBinary lets one helper serve several targets at once: discovery
// launches every binary in a directory with the same environment, so the name
// has to come from the file rather than from a variable shared by all of them.
func nameFromBinary(path string) string {
	base := strings.TrimSuffix(filepath.Base(path), ".exe")
	if strings.HasPrefix(base, BinaryPrefix) {
		return strings.TrimPrefix(base, BinaryPrefix)
	}
	return "fake"
}

// helperEmitter echoes what it was given, so a test asserting on its output is
// asserting on the boundary rather than on a mapping table.
type helperEmitter struct {
	name   string
	refuse bool
}

func (h helperEmitter) Name() string                     { return h.name }
func (h helperEmitter) SupportedResourceTypes() []string { return []string{"package", "service"} }
func (h helperEmitter) Capabilities() sdk.Capabilities {
	return sdk.Capabilities{NativeNotify: true, RuntimeCondition: true}
}

func (h helperEmitter) Emit(g *sdk.ResourceGraph, data sdk.Data) (sdk.Artifact, []sdk.Warning, error) {
	if h.refuse {
		return sdk.Artifact{}, nil, &sdk.CompileError{
			Resource: "nginx_pkg",
			Rule:     "unsupported-resource-type",
			Msg:      "this target maps nothing at all",
			Pos:      sdk.Position{File: "site.yaml", Line: 4, Column: 3},
		}
	}
	return sdk.Artifact{Files: map[string]string{
		"tree.txt": g.Canonical(),
		"data.txt": sdk.CanonicalValue(map[string]any(data)),
	}}, []sdk.Warning{{Target: h.name, Msg: "echoed"}}, nil
}

// testBinary is this test binary, which doubles as the plugin.
func testBinary() string { return os.Args[0] }

// startHelper launches the test binary as a plugin in the given mode.
func startHelper(t *testing.T, mode string, env ...string) (*Client, error) {
	t.Helper()
	return Start(context.Background(), testBinary(),
		WithArgs("-test.run=TestPluginHelperProcess"),
		WithEnv(append([]string{helperEnv + "=" + mode}, env...)...))
}

func mustStartHelper(t *testing.T, mode string, env ...string) *Client {
	t.Helper()
	c, err := startHelper(t, mode, env...)
	if err != nil {
		t.Fatalf("starting the helper plugin: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}

func sampleGraph() *sdk.ResourceGraph {
	return &sdk.ResourceGraph{
		Name:   "web",
		Host:   "web01.example.com",
		Target: "fake",
		Resources: []sdk.Resource{
			{ID: "nginx_pkg", Type: "package", State: "present",
				Params: map[string]any{"name": "nginx", "port": 8080},
				Index:  0, Pos: sdk.Position{File: "site.yaml", Line: 4, Column: 3}},
			{ID: "nginx_svc", Type: "service", State: "running",
				Params: map[string]any{"name": "nginx"}, Index: 1},
		},
		Edges: []sdk.Edge{
			{From: "nginx_pkg", To: "nginx_svc", Kind: sdk.HardOrder},
			{From: "nginx_pkg", To: "nginx_svc", Kind: sdk.Notify, Action: "restart"},
		},
	}
}

func containsAll(s string, parts ...string) bool {
	for _, p := range parts {
		if !strings.Contains(s, p) {
			return false
		}
	}
	return true
}
