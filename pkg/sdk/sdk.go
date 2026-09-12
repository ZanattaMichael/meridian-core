// Package sdk is the public contract a Meridian target plugin implements.
//
// A plugin is a separate binary. It implements Emitter over Meridian's own
// resource tree and calls Serve; the host launches it, negotiates a protocol
// version, and drives it over gRPC. Everything a third party needs lives here
// and under pluginpb, because internal/ is unreachable from outside this
// module by design.
//
// The types in this package are deliberately plain data with exported fields.
// The compiler's own tree (internal/ast) keeps its invariants private because
// it builds them; by the time a tree reaches a plugin those decisions are
// already made, and a plugin author should be able to read the whole input
// without calling an accessor for every field.
//
// What crosses this boundary is a compiled document, not a source one: every
// compile-time `when` has been evaluated and pruned, the resources are in a
// deterministic apply order, and the target host is a literal. A plugin's job
// is mapping and serialisation, never evaluation.
package sdk

// ProtocolVersion is the wire protocol this SDK speaks. A host refuses a plugin
// reporting a version it does not implement, rather than guessing at the
// difference.
//
// It is a separate number from the SDK's release version on purpose: the SDK
// gains helpers far more often than the wire format changes, and a plugin built
// against an older SDK stays loadable for as long as the protocol number holds.
const ProtocolVersion uint32 = 1

// Version is the SDK release a plugin was compiled against. It is reported over
// the handshake rather than inferred by the host, per the release policy, so a
// version mismatch is a fact the host reads rather than a guess it makes.
const Version = "0.1.0"

// HandshakeMagic opens the single line a plugin writes to stdout once it is
// listening. The host reads that line to learn where to dial; anything else on
// stdout before it means the binary is not a Meridian plugin, which is a much
// clearer failure than a dial timeout.
const HandshakeMagic = "MERIDIAN-PLUGIN"
