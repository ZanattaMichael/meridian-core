package puppet

import (
	"fmt"
	"strings"
)

// validate applies the constraints that are Puppet's rather than Meridian's,
// before anything is serialised. Every failure names the offending resource and
// the rule it broke, so the message points at the line the author must change.
func validate(m *manifest) error {
	if m.Host == "" {
		return &Error{Target: Name, Rule: "missing-host", Msg: "manifest has no node to assign the class to"}
	}

	// Puppet requires a title to be unique for its type across the whole
	// catalog, and a duplicate is a compile error on the primary rather than a
	// warning. Meridian resource ids are already unique within a document, so
	// this catches a tree assembled some other way rather than ordinary input.
	titles := make(map[string]string, len(m.Resources))
	// The namevar check is the one that catches real documents. Two resources
	// with different ids can still name the same package or the same path, and
	// Puppet only discovers that at apply time, on the node, as a conflict
	// between two declarations the operator has to trace back by hand.
	names := make(map[string]string, len(m.Resources))

	declared := make(map[string]bool, len(m.Resources))
	for _, r := range m.Resources {
		declared[r.ResourceID] = true
	}

	for _, r := range m.Resources {
		if err := checkTitle(r); err != nil {
			return err
		}
		key := r.Type + "/" + r.Title
		if prev, dup := titles[key]; dup {
			return &Error{
				Target: Name, Resource: r.ResourceID,
				Rule: "duplicate-title", Pos: r.Pos,
				Msg: fmt.Sprintf("a %s titled %q is already declared by resource %q; "+
					"Puppet requires a title to be unique for its type across the catalog",
					r.Type, r.Title, prev),
			}
		}
		titles[key] = r.ResourceID

		if nv, ok := namevarOf(r); ok {
			nk := r.Type + "/" + nv
			if prev, dup := names[nk]; dup {
				return &Error{
					Target: Name, Resource: r.ResourceID,
					Rule: "duplicate-namevar", Pos: r.Pos,
					Msg: fmt.Sprintf("this %s manages %q, which resource %q already manages; "+
						"two declarations of one thing is a conflict Puppet reports on the "+
						"node at apply time, not here, so it is refused now",
						r.Type, nv, prev),
				}
			}
			names[nk] = r.ResourceID
		}

		if len(r.Attrs) == 0 {
			return &Error{
				Target: Name, Resource: r.ResourceID,
				Rule: "empty-resource", Pos: r.Pos,
				Msg: fmt.Sprintf("%s resource %q was given no attributes", r.Type, r.Title),
			}
		}

		for _, ref := range append(append([]reference{}, r.Require...), r.Notify...) {
			if !declared[ref.Title] {
				return &Error{
					Target: Name, Resource: r.ResourceID,
					Rule: "dangling-reference", Pos: r.Pos,
					Msg: fmt.Sprintf("references %s, which this manifest does not declare", ref),
				}
			}
		}
	}
	return nil
}

// checkTitle rejects a title Puppet could not parse inside a declaration. A
// title is written between single quotes, so a quote or a newline in one would
// change the manifest's structure rather than merely look wrong.
func checkTitle(r resource) error {
	switch {
	case strings.TrimSpace(r.Title) == "":
		return &Error{
			Target: Name, Resource: r.ResourceID,
			Rule: "empty-title", Pos: r.Pos,
			Msg: "resource has an empty title",
		}
	case strings.ContainsAny(r.Title, "\n\r"):
		return &Error{
			Target: Name, Resource: r.ResourceID,
			Rule: "multiline-title", Pos: r.Pos,
			Msg: fmt.Sprintf("resource title %q spans more than one line", r.Title),
		}
	case strings.Contains(r.Title, "'"):
		return &Error{
			Target: Name, Resource: r.ResourceID,
			Rule: "quoted-title", Pos: r.Pos,
			Msg: fmt.Sprintf("resource title %q contains a single quote, which would "+
				"terminate the title Puppet reads", r.Title),
		}
	}
	return nil
}

// namevarOf returns the value Puppet identifies the resource by, when it is a
// string. A non-string namevar is left alone rather than compared by formatting.
func namevarOf(r resource) (string, bool) {
	attr, ok := namevars[r.Type]
	if !ok {
		return "", false
	}
	v, ok := r.Attrs[attr]
	if !ok {
		return "", false
	}
	s, ok := v.(string)
	return s, ok
}

// namevars is the native attribute each Puppet type identifies its subject by,
// derived from the mapping table so the two cannot drift apart.
var namevars = func() map[string]string {
	out := make(map[string]string, len(mappings))
	for _, m := range mappings {
		if m.namevar == "" {
			continue
		}
		out[m.puppetType] = m.params[m.namevar]
	}
	return out
}()
