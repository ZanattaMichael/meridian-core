package sdk

import (
	"fmt"

	pb "github.com/ZanattaMichael/meridian-core/pkg/sdk/pluginpb"
)

// This file is the only place the protobuf shapes and the Go shapes meet.
// Keeping the conversion in one file means a protocol change has exactly one
// place to break, and neither an emitter nor a host ever handles a pb type.

func positionToWire(p Position) *pb.Position {
	return &pb.Position{
		File:   p.File,
		DocIdx: int32(p.DocIdx),
		Line:   int32(p.Line),
		Column: int32(p.Column),
	}
}

func positionFromWire(p *pb.Position) Position {
	if p == nil {
		return Position{}
	}
	return Position{
		File:   p.GetFile(),
		DocIdx: int(p.GetDocIdx()),
		Line:   int(p.GetLine()),
		Column: int(p.GetColumn()),
	}
}

func edgeKindToWire(k EdgeKind) pb.EdgeKind {
	if k == Notify {
		return pb.EdgeKind_EDGE_KIND_NOTIFY
	}
	return pb.EdgeKind_EDGE_KIND_HARD_ORDER
}

// edgeKindFromWire refuses an unspecified kind rather than defaulting to one.
// Guessing between an ordering constraint and a deferred trigger would change
// what the document means.
func edgeKindFromWire(k pb.EdgeKind) (EdgeKind, error) {
	switch k {
	case pb.EdgeKind_EDGE_KIND_HARD_ORDER:
		return HardOrder, nil
	case pb.EdgeKind_EDGE_KIND_NOTIFY:
		return Notify, nil
	default:
		return 0, fmt.Errorf("edge kind %v is not a kind this protocol version defines", k)
	}
}

// GraphToWire converts a resource tree into its protocol form.
func GraphToWire(g *ResourceGraph) (*pb.ResourceGraph, error) {
	if g == nil {
		return nil, fmt.Errorf("cannot send a nil resource graph")
	}
	out := &pb.ResourceGraph{Name: g.Name, Host: g.Host, Target: g.Target}
	for _, r := range g.Resources {
		params, err := encodeValues(r.Params)
		if err != nil {
			return nil, fmt.Errorf("encoding params of resource %q: %w", r.ID, err)
		}
		out.Resources = append(out.Resources, &pb.Resource{
			Id:          r.ID,
			Type:        r.Type,
			State:       r.State,
			ParamsJson:  params,
			RuntimeWhen: r.RuntimeWhen,
			Index:       int32(r.Index),
			Pos:         positionToWire(r.Pos),
		})
	}
	for _, e := range g.Edges {
		out.Edges = append(out.Edges, &pb.Edge{
			From:   e.From,
			To:     e.To,
			Kind:   edgeKindToWire(e.Kind),
			Action: e.Action,
			Pos:    positionToWire(e.Pos),
		})
	}
	return out, nil
}

// GraphFromWire converts a protocol tree back into the SDK's own types.
func GraphFromWire(g *pb.ResourceGraph) (*ResourceGraph, error) {
	if g == nil {
		return nil, fmt.Errorf("received no resource graph")
	}
	out := &ResourceGraph{Name: g.GetName(), Host: g.GetHost(), Target: g.GetTarget()}
	for _, r := range g.GetResources() {
		params, err := decodeValues(r.GetParamsJson())
		if err != nil {
			return nil, fmt.Errorf("resource %q: %w", r.GetId(), err)
		}
		out.Resources = append(out.Resources, Resource{
			ID:          r.GetId(),
			Type:        r.GetType(),
			State:       r.GetState(),
			Params:      params,
			RuntimeWhen: r.GetRuntimeWhen(),
			Index:       int(r.GetIndex()),
			Pos:         positionFromWire(r.GetPos()),
		})
	}
	for _, e := range g.GetEdges() {
		kind, err := edgeKindFromWire(e.GetKind())
		if err != nil {
			return nil, fmt.Errorf("edge %s -> %s: %w", e.GetFrom(), e.GetTo(), err)
		}
		out.Edges = append(out.Edges, Edge{
			From:   e.GetFrom(),
			To:     e.GetTo(),
			Kind:   kind,
			Action: e.GetAction(),
			Pos:    positionFromWire(e.GetPos()),
		})
	}
	return out, nil
}

func capabilitiesToWire(c Capabilities) *pb.Capabilities {
	return &pb.Capabilities{
		NativeNotify:     c.NativeNotify,
		SyntheticNotify:  c.SyntheticNotify,
		RuntimeCondition: c.RuntimeCondition,
	}
}

func capabilitiesFromWire(c *pb.Capabilities) Capabilities {
	if c == nil {
		return Capabilities{}
	}
	return Capabilities{
		NativeNotify:     c.GetNativeNotify(),
		SyntheticNotify:  c.GetSyntheticNotify(),
		RuntimeCondition: c.GetRuntimeCondition(),
	}
}

func artifactToWire(a Artifact) *pb.Artifact {
	files := make(map[string]string, len(a.Files))
	for k, v := range a.Files {
		files[k] = v
	}
	return &pb.Artifact{Files: files}
}

func artifactFromWire(a *pb.Artifact) Artifact {
	if a == nil {
		return Artifact{}
	}
	files := make(map[string]string, len(a.GetFiles()))
	for k, v := range a.GetFiles() {
		files[k] = v
	}
	return Artifact{Files: files}
}

func warningsToWire(ws []Warning) []*pb.Warning {
	out := make([]*pb.Warning, 0, len(ws))
	for _, w := range ws {
		out = append(out, &pb.Warning{Target: w.Target, Resource: w.Resource, Msg: w.Msg})
	}
	return out
}

func warningsFromWire(ws []*pb.Warning) []Warning {
	if len(ws) == 0 {
		return nil
	}
	out := make([]Warning, 0, len(ws))
	for _, w := range ws {
		out = append(out, Warning{Target: w.GetTarget(), Resource: w.GetResource(), Msg: w.GetMsg()})
	}
	return out
}

// compileErrorToWire flattens any error an emitter returned. An emitter that
// returns a plain error still crosses the boundary usefully; it just arrives
// without a resource or a position, which is the emitter's omission rather than
// the protocol's.
func compileErrorToWire(err error) *pb.CompileError {
	if err == nil {
		return nil
	}
	var ce *CompileError
	if as(err, &ce) {
		return &pb.CompileError{
			Resource: ce.Resource,
			Rule:     ce.Rule,
			Msg:      ce.Msg,
			Pos:      positionToWire(ce.Pos),
		}
	}
	return &pb.CompileError{Msg: err.Error()}
}

// compileErrorFromWire rebuilds the error, stamping the target that raised it.
func compileErrorFromWire(e *pb.CompileError, targetName string) error {
	if e == nil {
		return nil
	}
	return &CompileError{
		Target:   targetName,
		Resource: e.GetResource(),
		Rule:     e.GetRule(),
		Msg:      e.GetMsg(),
		Pos:      positionFromWire(e.GetPos()),
	}
}

// EmitRequest builds the protocol request carrying a tree and its resolved
// data. It is exported because the host side of the protocol lives in a
// different package and should not be reimplementing the encoding.
func EmitRequest(g *ResourceGraph, data Data) (*pb.EmitRequest, error) {
	wire, err := GraphToWire(g)
	if err != nil {
		return nil, err
	}
	encoded, err := encodeValues(data)
	if err != nil {
		return nil, fmt.Errorf("encoding resolved data: %w", err)
	}
	return &pb.EmitRequest{Graph: wire, DataJson: encoded}, nil
}

// EmitResponse converts a protocol response back into SDK values. A compile
// failure carried in the response becomes a *CompileError, stamped with the
// target that raised it.
func EmitResponse(resp *pb.EmitResponse, targetName string) (Artifact, []Warning, error) {
	if resp == nil {
		return Artifact{}, nil, fmt.Errorf("the plugin returned no response")
	}
	if err := compileErrorFromWire(resp.GetError(), targetName); err != nil {
		return Artifact{}, nil, err
	}
	return artifactFromWire(resp.GetArtifact()), warningsFromWire(resp.GetWarnings()), nil
}
