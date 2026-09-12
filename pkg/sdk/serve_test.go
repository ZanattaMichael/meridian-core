package sdk

import (
	"bufio"
	"context"
	"errors"
	"io"
	"net"
	"strings"
	"testing"
	"time"

	pb "github.com/ZanattaMichael/meridian-core/pkg/sdk/pluginpb"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// fakeEmitter is a target with no opinions, so a test of the transport is not
// also a test of a mapping table.
type fakeEmitter struct {
	name string
	err  error
	warn []Warning
}

func (f fakeEmitter) Name() string                     { return f.name }
func (f fakeEmitter) SupportedResourceTypes() []string { return []string{"package", "service"} }
func (f fakeEmitter) Capabilities() Capabilities {
	return Capabilities{NativeNotify: true, RuntimeCondition: true}
}

func (f fakeEmitter) Emit(g *ResourceGraph, data Data) (Artifact, []Warning, error) {
	if f.err != nil {
		return Artifact{}, nil, f.err
	}
	return Artifact{Files: map[string]string{
		"tree.txt": g.Canonical(),
		"data.txt": CanonicalValue(map[string]any(data)),
	}}, f.warn, nil
}

// servePlugin runs an emitter over the real protocol inside the test process
// and returns a client for it. Everything except the process boundary is
// exercised, which is what keeps these tests fast enough to run on every build.
func servePlugin(t *testing.T, e Emitter) pb.EmitterClient {
	t.Helper()

	announceR, announceW := io.Pipe()
	doneR, doneW := io.Pipe()

	served := make(chan error, 1)
	go func() { served <- serve(e, announceW, doneR) }()

	line, err := bufio.NewReader(announceR).ReadString('\n')
	if err != nil {
		t.Fatalf("reading the handshake: %v", err)
	}
	parts := strings.Split(strings.TrimSpace(line), "|")
	if len(parts) != 5 || parts[0] != HandshakeMagic {
		t.Fatalf("handshake = %q", line)
	}
	network, address := parts[3], parts[4]

	conn, err := grpc.NewClient("passthrough:///"+address,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithContextDialer(func(ctx context.Context, addr string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, network, addr)
		}))
	if err != nil {
		t.Fatalf("dialing the plugin: %v", err)
	}

	t.Cleanup(func() {
		_ = conn.Close()
		_ = doneW.Close()
		select {
		case err := <-served:
			if err != nil {
				t.Errorf("Serve returned %v", err)
			}
		case <-time.After(5 * time.Second):
			t.Error("Serve did not return after its stdin closed")
		}
		_ = announceR.Close()
	})
	return pb.NewEmitterClient(conn)
}

func TestServeDescribesTheEmitter(t *testing.T) {
	client := servePlugin(t, fakeEmitter{name: "fake"})

	resp, err := client.Describe(context.Background(), &pb.DescribeRequest{})
	if err != nil {
		t.Fatalf("Describe: %v", err)
	}
	if resp.GetName() != "fake" {
		t.Fatalf("name = %q", resp.GetName())
	}
	if resp.GetProtocolVersion() != ProtocolVersion || resp.GetSdkVersion() != Version {
		t.Fatalf("handshake reported protocol %d / sdk %q", resp.GetProtocolVersion(), resp.GetSdkVersion())
	}
	if !resp.GetCapabilities().GetNativeNotify() || resp.GetCapabilities().GetSyntheticNotify() {
		t.Fatalf("capabilities = %+v", resp.GetCapabilities())
	}
	if len(resp.GetResourceTypes()) != 2 {
		t.Fatalf("resource types = %v", resp.GetResourceTypes())
	}
}

func TestServeRoundTripsATree(t *testing.T) {
	client := servePlugin(t, fakeEmitter{name: "fake", warn: []Warning{{Target: "fake", Msg: "flattened"}}})

	want := sampleGraph()
	req, err := EmitRequest(want, Data{"http_port": 8080})
	if err != nil {
		t.Fatalf("EmitRequest: %v", err)
	}
	resp, err := client.Emit(context.Background(), req)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	artifact, warnings, err := EmitResponse(resp, "fake")
	if err != nil {
		t.Fatalf("EmitResponse: %v", err)
	}

	// The emitter echoes what it received, so this compares the tree the plugin
	// saw against the tree the host sent.
	if got := artifact.Files["tree.txt"]; got != want.Canonical() {
		t.Fatalf("the plugin saw a different tree:\n%s\n---\n%s", got, want.Canonical())
	}
	if got := artifact.Files["data.txt"]; got != `{"http_port":8080}` {
		t.Fatalf("the plugin saw data %s", got)
	}
	if len(warnings) != 1 || warnings[0].Msg != "flattened" {
		t.Fatalf("warnings = %+v", warnings)
	}
}

// TestACompileFailureIsNotATransportFailure is the distinction the protocol
// draws deliberately: a target refusing a document is the capability contract
// working, and a host must be able to tell it from a plugin falling over.
func TestACompileFailureIsNotATransportFailure(t *testing.T) {
	refusal := &CompileError{Resource: "r", Rule: "unsupported-resource-type", Msg: "no", Pos: Position{File: "site.yaml", Line: 7}}
	client := servePlugin(t, fakeEmitter{name: "fake", err: refusal})

	req, err := EmitRequest(sampleGraph(), nil)
	if err != nil {
		t.Fatalf("EmitRequest: %v", err)
	}
	resp, err := client.Emit(context.Background(), req)
	if err != nil {
		t.Fatalf("a refusal should arrive as a response, not a status error: %v", err)
	}

	_, _, err = EmitResponse(resp, "fake")
	var got *CompileError
	if !errors.As(err, &got) {
		t.Fatalf("error = %T, want a *CompileError", err)
	}
	if got.Rule != "unsupported-resource-type" || got.Pos.Line != 7 {
		t.Fatalf("the refusal lost its provenance: %+v", got)
	}
}

// TestAMalformedRequestIsATransportFailure is the other half of that line.
func TestAMalformedRequestIsATransportFailure(t *testing.T) {
	client := servePlugin(t, fakeEmitter{name: "fake"})

	_, err := client.Emit(context.Background(), &pb.EmitRequest{
		Graph:    &pb.ResourceGraph{Resources: []*pb.Resource{{Id: "r", ParamsJson: []byte("{not json")}}},
		DataJson: nil,
	})
	if err == nil {
		t.Fatal("a malformed graph should fail at the transport level")
	}
}

func TestServeRefusesANilEmitter(t *testing.T) {
	if err := Serve(nil); err == nil {
		t.Fatal("serving nothing should be an error")
	}
}
