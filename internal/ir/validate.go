package ir

import "fmt"

// Bundle is a set of documents loaded together. Cross-document references
// (`targetHost.from: infrastructure`) resolve within a bundle and nowhere else.
type Bundle struct {
	Documents []Document
}

// NewBundle groups already-parsed documents. It performs no validation; call
// Validate for that.
func NewBundle(docs ...Document) *Bundle {
	return &Bundle{Documents: docs}
}

// Load parses every path and returns the combined bundle. Parse failures from
// all files are reported together rather than stopping at the first bad file.
func Load(paths ...string) (*Bundle, error) {
	var docs []Document
	var errs ErrorList
	for _, p := range paths {
		parsed, err := ParseFile(p)
		if err != nil {
			collect(&errs, err)
			continue
		}
		docs = append(docs, parsed...)
	}
	if err := errs.err(); err != nil {
		return nil, err
	}
	return &Bundle{Documents: docs}, nil
}

// collect flattens an ErrorList into errs, wrapping anything else.
func collect(errs *ErrorList, err error) {
	switch e := err.(type) {
	case ErrorList:
		*errs = append(*errs, e...)
	case *Error:
		errs.add(e)
	default:
		errs.add(&Error{Msg: err.Error(), Err: err})
	}
}

// Infrastructure returns the Infrastructure document with the given
// metadata.name.
func (b *Bundle) Infrastructure(name string) (*Infrastructure, bool) {
	for _, d := range b.Documents {
		if infra, ok := d.(*Infrastructure); ok && infra.Metadata.Name == name {
			return infra, true
		}
	}
	return nil, false
}

// ResourceSet returns the ResourceSet document with the given metadata.name.
func (b *Bundle) ResourceSet(name string) (*ResourceSet, bool) {
	for _, d := range b.Documents {
		if rs, ok := d.(*ResourceSet); ok && rs.Metadata.Name == name {
			return rs, true
		}
	}
	return nil, false
}

// Validate checks every document in the bundle plus the cross-document
// references between them, reporting all diagnostics at once.
func (b *Bundle) Validate() error {
	var errs ErrorList
	seen := map[string]Position{}
	for _, doc := range b.Documents {
		key := string(doc.KindOf()) + "/" + doc.NameOf()
		if prev, dup := seen[key]; dup {
			errs.add(errorf(doc.PositionOf(), "metadata.name",
				"duplicate %s document name %q (first declared at %s)", doc.KindOf(), doc.NameOf(), prev))
		} else {
			seen[key] = doc.PositionOf()
		}
		validateDocument(doc, &errs)
	}
	for _, doc := range b.Documents {
		rs, ok := doc.(*ResourceSet)
		if !ok {
			continue
		}
		b.validateTargetHost(rs, &errs)
	}
	return errs.err()
}

// Validate checks a single document in isolation. Cross-document references are
// not checked here; use Bundle.Validate for that.
func Validate(doc Document) error {
	var errs ErrorList
	validateDocument(doc, &errs)
	return errs.err()
}

func validateDocument(doc Document, errs *ErrorList) {
	pos := doc.PositionOf()
	if doc.NameOf() == "" {
		errs.add(errorf(pos, "metadata.name", "missing required field"))
	}
	if doc.TargetOf() == "" {
		errs.add(errorf(pos, "spec.target", "missing required field"))
	}
	resources := doc.ResourcesOf()
	if len(resources) == 0 {
		errs.add(errorf(pos, "spec.resources", "at least one resource is required"))
	}

	declared := make(map[string]Position, len(resources))
	for i := range resources {
		r := &resources[i]
		path := fmt.Sprintf("spec.resources[%d]", i)
		switch {
		case r.ID == "":
			errs.add(errorf(r.Pos, path+".id", "missing required field"))
		default:
			if prev, dup := declared[r.ID]; dup {
				errs.add(errorf(r.Pos, path+".id", "duplicate resource id %q (first declared at %s)", r.ID, prev))
			} else {
				declared[r.ID] = r.Pos
			}
		}
		if r.Type == "" {
			errs.add(errorf(r.Pos, path+".type", "missing required field"))
		}
	}

	for i := range resources {
		r := &resources[i]
		path := fmt.Sprintf("spec.resources[%d]", i)
		for j, dep := range r.DependsOn {
			p := fmt.Sprintf("%s.dependsOn[%d]", path, j)
			if err := checkIfMissing(dep.IfMissing, dep.Pos, p); err != nil {
				errs.add(err)
			}
			switch {
			case dep.Resource == "":
				errs.add(errorf(dep.Pos, p+".resource", "missing required field"))
			case dep.Resource == r.ID:
				errs.add(errorf(dep.Pos, p+".resource", "resource %q cannot depend on itself", r.ID))
			default:
				if _, ok := declared[dep.Resource]; !ok {
					errs.add(errorf(dep.Pos, p+".resource",
						"resource %q depends on unknown resource %q", r.ID, dep.Resource))
				}
			}
		}
		for j, n := range r.Notifies {
			p := fmt.Sprintf("%s.notifies[%d]", path, j)
			if err := checkIfMissing(n.IfMissing, n.Pos, p); err != nil {
				errs.add(err)
			}
			if n.Action == "" {
				errs.add(errorf(n.Pos, p+".action", "missing required field"))
			}
			switch {
			case n.Resource == "":
				errs.add(errorf(n.Pos, p+".resource", "missing required field"))
			case n.Resource == r.ID:
				errs.add(errorf(n.Pos, p+".resource", "resource %q cannot notify itself", r.ID))
			default:
				if _, ok := declared[n.Resource]; !ok {
					errs.add(errorf(n.Pos, p+".resource",
						"resource %q notifies unknown resource %q", r.ID, n.Resource))
				}
			}
		}
	}

	if infra, ok := doc.(*Infrastructure); ok {
		names := make(map[string]Position, len(infra.Spec.Outputs))
		for i, o := range infra.Spec.Outputs {
			p := fmt.Sprintf("spec.outputs[%d]", i)
			if o.Name == "" {
				errs.add(errorf(o.Pos, p+".name", "missing required field"))
				continue
			}
			if prev, dup := names[o.Name]; dup {
				errs.add(errorf(o.Pos, p+".name", "duplicate output name %q (first declared at %s)", o.Name, prev))
				continue
			}
			names[o.Name] = o.Pos
		}
	}
}

func checkIfMissing(v IfMissing, pos Position, path string) *Error {
	switch v {
	case "", IfMissingError, IfMissingSkip:
		return nil
	}
	return errorf(pos, path+".ifMissing", "unknown policy %q (expected %q or %q)", v, IfMissingError, IfMissingSkip)
}

// validateTargetHost resolves a ResourceSet's binding to the node it
// configures, including the cross-document lookup into a sibling
// Infrastructure document.
func (b *Bundle) validateTargetHost(rs *ResourceSet, errs *ErrorList) {
	th := rs.Spec.TargetHost
	if th == nil {
		return
	}
	switch th.From {
	case "":
		errs.add(errorf(th.Pos, "spec.targetHost.from", "missing required field"))
	case TargetHostFromInline:
		if th.Host == "" {
			errs.add(errorf(th.Pos, "spec.targetHost.host", "missing required field for from: inline"))
		}
	case TargetHostFromInfrastructure:
		if th.Ref == "" {
			errs.add(errorf(th.Pos, "spec.targetHost.ref", "missing required field for from: infrastructure"))
		}
		if th.Output == "" {
			errs.add(errorf(th.Pos, "spec.targetHost.output", "missing required field for from: infrastructure"))
		}
		if th.Ref == "" || th.Output == "" {
			return
		}
		infra, ok := b.Infrastructure(th.Ref)
		if !ok {
			errs.add(errorf(th.Pos, "spec.targetHost.ref",
				"no Infrastructure document named %q is loaded", th.Ref))
			return
		}
		if _, ok := infra.Output(th.Output); !ok {
			errs.add(errorf(th.Pos, "spec.targetHost.output",
				"Infrastructure %q declares no output %q", th.Ref, th.Output))
		}
	default:
		errs.add(errorf(th.Pos, "spec.targetHost.from",
			"unknown source %q (expected %q or %q)", th.From, TargetHostFromInfrastructure, TargetHostFromInline))
	}
}

// ResolveTargetHost returns the Infrastructure document and output a
// ResourceSet's targetHost refers to.
func (b *Bundle) ResolveTargetHost(rs *ResourceSet) (*Infrastructure, Output, error) {
	th := rs.Spec.TargetHost
	if th == nil || th.From != TargetHostFromInfrastructure {
		return nil, Output{}, errorf(rs.Pos, "spec.targetHost",
			"ResourceSet %q does not reference an Infrastructure document", rs.Metadata.Name)
	}
	infra, ok := b.Infrastructure(th.Ref)
	if !ok {
		return nil, Output{}, errorf(th.Pos, "spec.targetHost.ref",
			"no Infrastructure document named %q is loaded", th.Ref)
	}
	out, ok := infra.Output(th.Output)
	if !ok {
		return nil, Output{}, errorf(th.Pos, "spec.targetHost.output",
			"Infrastructure %q declares no output %q", th.Ref, th.Output)
	}
	return infra, out, nil
}
