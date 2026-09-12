// Package plugin is the host side of Meridian's plugin protocol.
//
// A target is a separate process. The host launches the binary, reads one
// handshake line telling it where the plugin is listening, negotiates the
// protocol version, and then talks to it over gRPC as though it were a local
// sdk.Emitter. Nothing above this package knows whether a target is a
// subprocess or compiled in: the registry hands back an sdk.Emitter either way.
//
// The reason for a process boundary at all is that a target's mapping tables
// are the part of Meridian most likely to be written by someone else. A plugin
// that panics, leaks memory, or is built against a different SDK cannot take
// the host down with it, and a third party can ship one without a fork.
package plugin

import (
	"fmt"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// BinaryPrefix is the naming convention a discoverable plugin binary follows.
// Discovery is by name rather than by executing every file in a directory,
// because running an unknown binary to ask what it is would be a poor trade.
const BinaryPrefix = "meridian-target-"

// Info is what a plugin reports about itself at handshake time.
type Info struct {
	// ProtocolVersion is the wire protocol the plugin speaks.
	ProtocolVersion uint32
	// SDKVersion is the SDK release it was compiled against, reported rather
	// than inferred.
	SDKVersion string
	// Name is the target name documents select with spec.target.
	Name string
	// ResourceTypes is every IR resource type the target maps.
	ResourceTypes []string
	Capabilities  sdk.Capabilities
	// Path is the binary the plugin was loaded from; empty for an in-process
	// emitter.
	Path string
}

// Error is a plugin-loading or plugin-transport failure.
//
// It is a distinct type from sdk.CompileError because the two mean opposite
// things: a CompileError is a target correctly refusing a document, while this
// is the plugin mechanism itself failing. Collapsing them would let a crashed
// binary read like a rejected resource.
type Error struct {
	Plugin string
	Path   string
	Msg    string
	// Stderr is whatever the plugin wrote before it failed, kept because a
	// plugin that dies during startup usually explains itself there and nowhere
	// else.
	Stderr string
	Err    error
}

func (e *Error) Error() string {
	msg := "plugin"
	if e.Plugin != "" {
		msg = fmt.Sprintf("plugin %q", e.Plugin)
	} else if e.Path != "" {
		msg = fmt.Sprintf("plugin %s", e.Path)
	}
	out := msg + ": " + e.Msg
	if e.Err != nil {
		out += ": " + e.Err.Error()
	}
	if e.Stderr != "" {
		out += "\nplugin stderr: " + e.Stderr
	}
	return out
}

func (e *Error) Unwrap() error { return e.Err }
