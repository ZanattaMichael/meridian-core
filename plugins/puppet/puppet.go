// Package puppet compiles a Meridian resource tree into a Puppet manifest and
// the node statement that binds it to a host.
//
// It is a target plugin: it depends on pkg/sdk and nothing else of Meridian's.
// That is the property milestone 4 is really testing. The Ansible emitter was
// written alongside the AST and could have been shaped by it without anyone
// noticing; this one was written against the SDK as milestone 3 left it, so
// anything it could not express would have been a hidden Ansible-shaped
// assumption rather than a Puppet limitation.
//
// The four compile stages run in the order design plan §11.3 mandates:
// transform → sort → validate → emit.
//
// # How this target differs from Ansible
//
// Puppet compiles a catalog rather than running a task list, and the difference
// shows up in three places that are worth reading before the mapping table.
//
// Ordering is declared, not implied. A manifest's file order means nothing, so
// every dependency edge becomes an explicit require metaparameter and the
// dependency graph survives into the output intact. The Ansible target has to
// flatten that graph into one sequence and warn that it discarded parallelism;
// this target has nothing to warn about, which is the clearest evidence that
// the flattening was Ansible's limitation rather than the AST's.
//
// Notification is a refresh, not a named handler. Puppet's notify metaparameter
// tells a resource that something it depends on changed, and the resource
// decides what to do about it. There is no way to say which of several actions
// to take, so this target accepts the actions Puppet can actually perform and
// refuses the rest rather than quietly upgrading a reload into a restart.
//
// There is no apply-time conditional at all. A catalog is compiled before it
// reaches the node, so runtimeWhen has nowhere to go and Capabilities reports
// RuntimeCondition false. A document that needs one fails here with an
// explanation instead of compiling to a manifest that silently ignores it.
package puppet

import (
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// Name is the target name a document selects with spec.target.
const Name = "puppet"

// resource is one native Puppet resource declaration: a type, a title, its
// attributes, and the metaparameters that place it in the catalog's graph. It
// is the native AST this target's transform stage produces and its emit stage
// serialises.
type resource struct {
	// Type is the native Puppet resource type, lower-case as it is written in a
	// declaration: package, file, service, user, group, exec.
	Type string
	// Title is the resource's title. Puppet requires it to be unique for its
	// type across the whole catalog, not merely within this manifest, which is
	// a stricter rule than any other target has.
	Title string
	// Attrs are the resource's attributes, keyed by native attribute name.
	// ensure lives here too, carried as a bareword because Puppet's ensure
	// values are unquoted words rather than strings.
	Attrs map[string]any
	// Require lists references this resource must be applied after.
	Require []reference
	// Notify lists references this resource refreshes when it changes.
	Notify []reference

	// ResourceID and Pos are provenance, kept so a validation failure can name
	// the resource the author wrote rather than the declaration it became.
	ResourceID string
	Pos        sdk.Position
}

// reference is a Puppet resource reference, written Type['title'] with the type
// capitalised. It is a type rather than a preformatted string so the emit stage
// stays the only place that decides how one is written.
type reference struct {
	Type  string
	Title string
}

// manifest is a whole compiled document: one class holding every resource, and
// the host the node statement assigns it to.
type manifest struct {
	// Class is the manifest's class name, derived from the document name.
	Class string
	// Host is the node the class is assigned to.
	Host      string
	Resources []resource
}

// bareword is a value Puppet expects unquoted: an ensure value, or a provider
// name. Quoting one would still parse, but it would not be what a Puppet author
// would have written, and this target's output is meant to be readable by one.
type bareword string
