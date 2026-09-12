package plugin

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

func TestParseHandshakeReadsAWellFormedLine(t *testing.T) {
	network, address, h, err := parseHandshake("MERIDIAN-PLUGIN|1|0.1.0|unix|/tmp/x/plugin.sock")
	if err != nil {
		t.Fatalf("parseHandshake: %v", err)
	}
	if network != "unix" || address != "/tmp/x/plugin.sock" {
		t.Fatalf("dial target = %s %s", network, address)
	}
	if h.ProtocolVersion != 1 || h.SDKVersion != "0.1.0" {
		t.Fatalf("handshake = %+v", h)
	}
}

func TestParseHandshakeRejectsAnythingElse(t *testing.T) {
	for name, line := range map[string]string{
		"empty":          "",
		"not a plugin":   "Usage: some-tool [options]",
		"wrong magic":    "OTHER-PLUGIN|1|0.1.0|unix|/tmp/p.sock",
		"too few fields": "MERIDIAN-PLUGIN|1|0.1.0|unix",
		"bad version":    "MERIDIAN-PLUGIN|one|0.1.0|unix|/tmp/p.sock",
		"no address":     "MERIDIAN-PLUGIN|1|0.1.0|unix|",
	} {
		t.Run(name, func(t *testing.T) {
			if _, _, _, err := parseHandshake(line); err == nil {
				t.Fatalf("parseHandshake(%q) accepted a line it should have refused", line)
			}
		})
	}
}

func TestStartCompletesTheHandshakeAndDescribes(t *testing.T) {
	c := mustStartHelper(t, "serve")

	info := c.Info()
	if info.Name != "fake" {
		t.Fatalf("name = %q", info.Name)
	}
	if info.ProtocolVersion != sdk.ProtocolVersion {
		t.Fatalf("protocol version = %d, want %d", info.ProtocolVersion, sdk.ProtocolVersion)
	}
	if info.SDKVersion != sdk.Version {
		t.Fatalf("sdk version = %q, want %q", info.SDKVersion, sdk.Version)
	}
	if info.Path == "" {
		t.Fatal("info should record the binary it was loaded from")
	}
	if got := c.SupportedResourceTypes(); len(got) != 2 || got[0] != "package" {
		t.Fatalf("resource types = %v", got)
	}
	want := sdk.Capabilities{NativeNotify: true, RuntimeCondition: true}
	if c.Capabilities() != want {
		t.Fatalf("capabilities = %+v, want %+v", c.Capabilities(), want)
	}
}

// TestSupportedResourceTypesIsACopy: the list is shared state the caller must
// not be able to edit out from under the registry.
func TestSupportedResourceTypesIsACopy(t *testing.T) {
	c := mustStartHelper(t, "serve")
	c.SupportedResourceTypes()[0] = "tampered"
	if got := c.SupportedResourceTypes(); got[0] != "package" {
		t.Fatalf("a caller mutated the plugin's declared types: %v", got)
	}
}

// TestTheTreeSurvivesTheBoundaryUnchanged is the property the whole protocol
// exists to preserve: what the host compiled is what the plugin emitted from,
// down to the integer 8080 not becoming 8080.0.
func TestTheTreeSurvivesTheBoundaryUnchanged(t *testing.T) {
	c := mustStartHelper(t, "serve")
	g := sampleGraph()

	art, warnings, err := c.Emit(g, sdk.Data{"env": "prod", "replicas": 3})
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	if got := art.Files["tree.txt"]; got != g.Canonical() {
		t.Fatalf("the tree changed in transit:\n--- got ---\n%s\n--- want ---\n%s", got, g.Canonical())
	}
	if got, want := art.Files["data.txt"], `{"env":"prod","replicas":3}`; got != want {
		t.Fatalf("data = %s, want %s", got, want)
	}
	if len(warnings) != 1 || warnings[0].Msg != "echoed" || warnings[0].Target != "fake" {
		t.Fatalf("warnings = %v", warnings)
	}
}

func TestEmitAcceptsANilTreeAsAnError(t *testing.T) {
	c := mustStartHelper(t, "serve")
	_, _, err := c.Emit(nil, nil)
	if err == nil {
		t.Fatal("emitting a nil tree should fail rather than compile nothing successfully")
	}
	// Caught before the call is made: a request that cannot be encoded is the
	// host's own fault, not the plugin's.
	var pe *Error
	if !errors.As(err, &pe) || !strings.Contains(pe.Msg, "encode") {
		t.Fatalf("error = %v", err)
	}
}

// TestARefusalArrivesAsACompileError draws the line the protocol is built
// around: a target rejecting a document is a compile error with a position,
// not a transport failure.
func TestARefusalArrivesAsACompileError(t *testing.T) {
	c := mustStartHelper(t, "refuse")

	_, _, err := c.Emit(sampleGraph(), nil)
	var ce *sdk.CompileError
	if !errors.As(err, &ce) {
		t.Fatalf("error = %#v, want an *sdk.CompileError", err)
	}
	if ce.Rule != "unsupported-resource-type" || ce.Resource != "nginx_pkg" {
		t.Fatalf("compile error = %+v", ce)
	}
	if ce.Target != "fake" {
		t.Fatalf("target = %q; the host names the plugin that refused", ce.Target)
	}
	if ce.Pos != (sdk.Position{File: "site.yaml", Line: 4, Column: 3}) {
		t.Fatalf("position = %+v; a refusal has to point at the source", ce.Pos)
	}
	var pe *Error
	if errors.As(err, &pe) {
		t.Fatalf("a refusal was reported as a plugin fault: %v", err)
	}
}

func TestAProtocolMismatchIsRefusedBeforeAnyCall(t *testing.T) {
	_, err := startHelper(t, "wrong-protocol")
	if err == nil {
		t.Fatal("a plugin speaking another protocol version should not load")
	}
	if !containsAll(err.Error(), "protocol", "rebuild") {
		t.Fatalf("error should say what to do about it: %v", err)
	}
}

func TestABinaryThatIsNotAPluginIsRejected(t *testing.T) {
	_, err := startHelper(t, "noise")
	if err == nil {
		t.Fatal("a binary printing ordinary output should not be loaded as a plugin")
	}
	if !strings.Contains(err.Error(), "not a Meridian plugin handshake") {
		t.Fatalf("error = %v", err)
	}
}

// TestACrashingPluginSurfacesItsStderr: a plugin that dies during startup
// explains itself on stderr and nowhere else, so the host has to carry it.
func TestACrashingPluginSurfacesItsStderr(t *testing.T) {
	_, err := startHelper(t, "crash")
	if err == nil {
		t.Fatal("a plugin that exits immediately should fail to load")
	}
	var pe *Error
	if !errors.As(err, &pe) {
		t.Fatalf("error = %#v, want a *plugin.Error", err)
	}
	if !strings.Contains(pe.Stderr, "could not read its configuration") {
		t.Fatalf("stderr was not carried: %q", pe.Stderr)
	}
	if !strings.Contains(err.Error(), "could not read its configuration") {
		t.Fatalf("stderr should appear in the rendered error: %v", err)
	}
}

func TestAPluginThatNeverAnnouncesItselfTimesOut(t *testing.T) {
	start := time.Now()
	_, err := Start(context.Background(), testBinary(),
		WithArgs("-test.run=TestPluginHelperProcess"),
		WithEnv(helperEnv+"=silent"),
		WithHandshakeTimeout(250*time.Millisecond))
	if err == nil {
		t.Fatal("a silent plugin should not load")
	}
	if !strings.Contains(err.Error(), "no handshake line within") {
		t.Fatalf("error = %v", err)
	}
	if elapsed := time.Since(start); elapsed > 10*time.Second {
		t.Fatalf("the host waited %s, so the timeout is not what ended it", elapsed)
	}
}

func TestStartHonoursACancelledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := Start(ctx, testBinary(),
		WithArgs("-test.run=TestPluginHelperProcess"),
		WithEnv(helperEnv+"=silent")); err == nil {
		t.Fatal("a cancelled context should abandon the load")
	}
}

// TestCloseIsIdempotent matters because both the registry and a caller holding
// the client may close it, and the second call must not report a failure.
func TestCloseIsIdempotent(t *testing.T) {
	c, err := startHelper(t, "serve")
	if err != nil {
		t.Fatalf("starting the helper plugin: %v", err)
	}
	if err := c.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := c.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
}

func TestEmitAfterCloseFailsRatherThanHangs(t *testing.T) {
	c, err := startHelper(t, "serve")
	if err != nil {
		t.Fatalf("starting the helper plugin: %v", err)
	}
	if err := c.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if _, _, err := c.EmitContext(ctx, sampleGraph(), nil); err == nil {
		t.Fatal("emitting through a closed client should fail")
	}
}
