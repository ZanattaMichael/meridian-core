package sdk

import (
	"reflect"
	"testing"

	pb "github.com/ZanattaMichael/meridian-core/pkg/sdk/pluginpb"
)

func sampleGraph() *ResourceGraph {
	return &ResourceGraph{
		Name:   "web",
		Host:   "web01.example.com",
		Target: "ansible",
		Resources: []Resource{
			{
				ID:    "nginx_pkg",
				Type:  "package",
				State: "present",
				Params: map[string]any{
					"name":    "nginx",
					"port":    8080,
					"ratio":   0.5,
					"enabled": true,
					"tags":    []any{"web", "edge"},
					"limits":  map[string]any{"open_files": 4096},
				},
				Index: 0,
				Pos:   Position{File: "site.yaml", Line: 4, Column: 3},
			},
			{
				ID:          "nginx_svc",
				Type:        "service",
				State:       "running",
				Params:      map[string]any{"name": "nginx"},
				RuntimeWhen: "ansible_facts['os_family'] == 'Debian'",
				Index:       1,
				Pos:         Position{File: "site.yaml", Line: 9, Column: 3},
			},
		},
		Edges: []Edge{
			{From: "nginx_pkg", To: "nginx_svc", Kind: HardOrder, Pos: Position{File: "site.yaml", Line: 11}},
			{From: "nginx_pkg", To: "nginx_svc", Kind: Notify, Action: "restart"},
		},
	}
}

// TestGraphSurvivesTheWireUnchanged is the property the whole boundary rests
// on: a plugin must receive exactly the tree the host compiled, or the golden
// files stop meaning anything.
func TestGraphSurvivesTheWireUnchanged(t *testing.T) {
	want := sampleGraph()

	encoded, err := GraphToWire(want)
	if err != nil {
		t.Fatalf("GraphToWire: %v", err)
	}
	got, err := GraphFromWire(encoded)
	if err != nil {
		t.Fatalf("GraphFromWire: %v", err)
	}

	if got.Canonical() != want.Canonical() {
		t.Fatalf("the tree changed crossing the wire:\n%s\n---\n%s", got.Canonical(), want.Canonical())
	}
	if !reflect.DeepEqual(got.Edges, want.Edges) {
		t.Fatalf("edges = %+v, want %+v", got.Edges, want.Edges)
	}
}

// TestIntegersStayIntegersAcrossTheWire is why params are JSON bytes rather
// than a protobuf Struct. Struct models every number as a double, so a port
// would come back as 8080.0 and the emitted YAML would change.
func TestIntegersStayIntegersAcrossTheWire(t *testing.T) {
	encoded, err := GraphToWire(sampleGraph())
	if err != nil {
		t.Fatalf("GraphToWire: %v", err)
	}
	got, err := GraphFromWire(encoded)
	if err != nil {
		t.Fatalf("GraphFromWire: %v", err)
	}

	params := got.Resources[0].Params
	port, ok := params["port"].(int64)
	if !ok {
		t.Fatalf("port = %#v (%T), want an integer", params["port"], params["port"])
	}
	if port != 8080 {
		t.Fatalf("port = %d, want 8080", port)
	}
	if ratio, ok := params["ratio"].(float64); !ok || ratio != 0.5 {
		t.Fatalf("ratio = %#v, want the float 0.5", params["ratio"])
	}
	if limits, ok := params["limits"].(map[string]any); !ok {
		t.Fatalf("limits = %#v, want a nested object", params["limits"])
	} else if _, ok := limits["open_files"].(int64); !ok {
		t.Fatalf("a nested number was not restored: %#v", limits["open_files"])
	}
}

// TestAnUnspecifiedEdgeKindIsRefused: guessing between an ordering constraint
// and a deferred trigger would silently change what a document means.
func TestAnUnspecifiedEdgeKindIsRefused(t *testing.T) {
	_, err := GraphFromWire(&pb.ResourceGraph{
		Edges: []*pb.Edge{{From: "a", To: "b", Kind: pb.EdgeKind_EDGE_KIND_UNSPECIFIED}},
	})
	if err == nil {
		t.Fatal("expected an unspecified edge kind to be refused")
	}
}

func TestNilGraphsAreRefusedOnBothSides(t *testing.T) {
	if _, err := GraphToWire(nil); err == nil {
		t.Fatal("GraphToWire(nil) should fail")
	}
	if _, err := GraphFromWire(nil); err == nil {
		t.Fatal("GraphFromWire(nil) should fail")
	}
}

// TestCompileErrorKeepsItsShapeAcrossTheWire: the point of carrying a failure
// as fields rather than a string is that a host can still report it against the
// author's own line.
func TestCompileErrorKeepsItsShapeAcrossTheWire(t *testing.T) {
	original := &CompileError{
		Resource: "nginx_conf",
		Rule:     "unsupported-param",
		Msg:      "nope",
		Pos:      Position{File: "site.yaml", Line: 12, Column: 3},
	}

	round := compileErrorFromWire(compileErrorToWire(original), "ansible")
	var got *CompileError
	if !as(round, &got) {
		t.Fatalf("error = %T, want a *CompileError", round)
	}
	if got.Resource != original.Resource || got.Rule != original.Rule || got.Pos != original.Pos {
		t.Fatalf("error lost its shape: %+v", got)
	}
	if got.Target != "ansible" {
		t.Fatalf("target = %q, want the plugin that raised it", got.Target)
	}
	want := `site.yaml:12:3: ansible: resource "nginx_conf" [unsupported-param]: nope`
	if got.Error() != want {
		t.Fatalf("rendered = %q, want %q", got.Error(), want)
	}
}

// TestAPlainErrorStillCrossesTheWire: an emitter that returns an ordinary error
// should still be reported, just without provenance it never supplied.
func TestAPlainErrorStillCrossesTheWire(t *testing.T) {
	round := compileErrorFromWire(compileErrorToWire(errPlain{}), "ansible")
	if round == nil || round.Error() != "ansible: something broke" {
		t.Fatalf("error = %v", round)
	}
}

type errPlain struct{}

func (errPlain) Error() string { return "something broke" }

func TestEmitResponseRefusesNothing(t *testing.T) {
	if _, _, err := EmitResponse(nil, "ansible"); err == nil {
		t.Fatal("a missing response should be an error, not an empty artifact")
	}
}

func TestEmitRequestCarriesDataAndGraph(t *testing.T) {
	req, err := EmitRequest(sampleGraph(), Data{"http_port": 8080})
	if err != nil {
		t.Fatalf("EmitRequest: %v", err)
	}
	data, err := decodeValues(req.GetDataJson())
	if err != nil {
		t.Fatalf("decoding the data: %v", err)
	}
	if got, ok := data["http_port"].(int64); !ok || got != 8080 {
		t.Fatalf("http_port = %#v, want the integer 8080", data["http_port"])
	}
	if req.GetGraph().GetName() != "web" {
		t.Fatalf("graph name = %q", req.GetGraph().GetName())
	}
}
