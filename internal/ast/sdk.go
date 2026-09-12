package ast

import (
	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// This file is the only seam between the compiler's own tree and the tree a
// target sees. The two are kept as separate types on purpose.
//
// The internal tree owns invariants it built: its resources are in an order it
// derived, its index map is consistent with that order, and its params are
// copies rather than aliases of the source document. Handing that type out as
// the public contract would publish those internals, and every later change to
// how the compiler represents a document would become a breaking change for
// every third-party plugin.

// ToSDK converts the compiled tree into the public form a target consumes.
//
// Params are deep-copied on the way out for the same reason they were copied on
// the way in: an emitter that rewrites a param must not be able to reach back
// into the compiler's own tree, and a compiled-in emitter has no process
// boundary to stop it.
func (g *ResourceGraph) ToSDK() *sdk.ResourceGraph {
	if g == nil {
		return nil
	}
	out := &sdk.ResourceGraph{
		Name:      g.Name,
		Host:      g.Host,
		Target:    g.Target,
		Resources: make([]sdk.Resource, 0, len(g.resources)),
		Edges:     make([]sdk.Edge, 0, len(g.edges)),
	}
	for _, r := range g.resources {
		out.Resources = append(out.Resources, sdk.Resource{
			ID:          r.ID,
			Type:        r.Type,
			State:       r.State,
			Params:      copyValue(r.Params).(map[string]any),
			RuntimeWhen: r.RuntimeWhen,
			Index:       r.Index,
			Pos:         positionToSDK(r.Pos),
		})
	}
	for _, e := range g.edges {
		out.Edges = append(out.Edges, sdk.Edge{
			From:   e.From,
			To:     e.To,
			Kind:   edgeKindToSDK(e.Kind),
			Action: e.Action,
			Pos:    positionToSDK(e.Pos),
		})
	}
	return out
}

func positionToSDK(p ir.Position) sdk.Position {
	return sdk.Position{File: p.File, DocIdx: p.DocIdx, Line: p.Line, Column: p.Column}
}

func edgeKindToSDK(k EdgeKind) sdk.EdgeKind {
	if k == Notify {
		return sdk.Notify
	}
	return sdk.HardOrder
}
