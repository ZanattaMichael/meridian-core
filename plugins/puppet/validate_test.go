package puppet

import (
	"strings"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/fixtures"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// TestTwoResourcesManagingOnePackageAreRefused is the check that catches real
// documents. Puppet only discovers two declarations of one package when the
// agent applies the catalog on the node, and reports it there as a conflict
// between titles the operator has to trace back to the document by hand.
func TestTwoResourcesManagingOnePackageAreRefused(t *testing.T) {
	err := emitError(t,
		ir.Resource{ID: "nginx_pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
		ir.Resource{ID: "web_server", Type: "package", Params: map[string]any{"name": "nginx"}},
	)
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "duplicate-namevar" {
		t.Fatalf("rule = %q, want duplicate-namevar", ce.Rule)
	}
	if !strings.Contains(ce.Msg, "nginx") || !strings.Contains(ce.Msg, "nginx_pkg") {
		t.Fatalf("the failure should name the package and the resource that already manages it: %q", ce.Msg)
	}
	if ce.Resource != "web_server" {
		t.Fatalf("reported against %q, want the second declaration", ce.Resource)
	}
}

// TestTwoFilesAtOnePathAreRefused proves the namevar check reads the mapping
// table rather than a hard-coded list of types: file identifies its subject by
// path, and path is what the IR calls it too.
func TestTwoFilesAtOnePathAreRefused(t *testing.T) {
	err := emitError(t,
		ir.Resource{ID: "conf", Type: "file", Params: map[string]any{"path": "/etc/app.conf", "content": "a\n"}},
		ir.Resource{ID: "conf_again", Type: "file", Params: map[string]any{"path": "/etc/app.conf", "content": "b\n"}},
	)
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "duplicate-namevar" || !strings.Contains(ce.Msg, "/etc/app.conf") {
		t.Fatalf("want the path named in a duplicate-namevar failure, got %v", ce)
	}
}

// TestTwoTypesMayManageTheSameName is the boundary of the previous check. A
// user called app and a package called app are different subjects, and Puppet
// scopes both titles and namevars per type.
func TestTwoTypesMayManageTheSameName(t *testing.T) {
	files := emit(t,
		ir.Resource{ID: "app_pkg", Type: "package", Params: map[string]any{"name": "app"}},
		ir.Resource{ID: "app_user", Type: "user", Params: map[string]any{"name": "app"}},
	)
	manifest := files[ManifestPath]
	for _, want := range []string{"package { 'app_pkg':", "user { 'app_user':"} {
		if !strings.Contains(manifest, want) {
			t.Fatalf("expected %q in:\n%s", want, manifest)
		}
	}
}

// TestADuplicateTitleIsRefused covers the constraint that is Puppet's own.
// Document ids are already unique, so reaching this means a tree assembled some
// other way, which is precisely what a tree arriving over the plugin boundary is.
func TestADuplicateTitleIsRefused(t *testing.T) {
	m := &manifest{Class: "meridian_web", Host: fixtures.Host, Resources: []resource{
		{Type: "package", Title: "app", ResourceID: "app", Attrs: map[string]any{"name": "app"}},
		{Type: "package", Title: "app", ResourceID: "app_two", Attrs: map[string]any{"name": "other"}},
	}}
	err := validate(m)
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "duplicate-title" {
		t.Fatalf("rule = %q, want duplicate-title", ce.Rule)
	}
	if !strings.Contains(ce.Msg, "catalog") {
		t.Fatalf("the message should say why Puppet cares: %q", ce.Msg)
	}
}

// TestATitleThatWouldBreakTheManifestIsRefused: a title is written between
// single quotes, so one containing a quote or a newline would change the
// structure of the file rather than merely look wrong in it.
func TestATitleThatWouldBreakTheManifestIsRefused(t *testing.T) {
	cases := map[string]string{
		"empty-title":     "",
		"multiline-title": "two\nlines",
		"quoted-title":    "it's",
	}
	for rule, title := range cases {
		t.Run(rule, func(t *testing.T) {
			err := validate(&manifest{Class: "meridian_web", Host: fixtures.Host, Resources: []resource{
				{Type: "package", Title: title, ResourceID: "r", Attrs: map[string]any{"name": "app"}},
			}})
			var ce *sdk.CompileError
			if !asCompileError(err, &ce) {
				t.Fatalf("error is not a compile error: %v", err)
			}
			if ce.Rule != rule {
				t.Fatalf("rule = %q, want %q", ce.Rule, rule)
			}
		})
	}
}

// TestAReferenceToSomethingUndeclaredIsRefused. Puppet fails a catalog holding
// a require to a resource nobody declared, so the failure exists either way;
// the only question is whether the operator reads it here or on the primary.
func TestAReferenceToSomethingUndeclaredIsRefused(t *testing.T) {
	for name, r := range map[string]resource{
		"require": {Type: "service", Title: "svc", ResourceID: "svc",
			Attrs:   map[string]any{"name": "app"},
			Require: []reference{{Type: "package", Title: "absent_pkg"}}},
		"notify": {Type: "file", Title: "conf", ResourceID: "conf",
			Attrs:  map[string]any{"path": "/etc/app.conf"},
			Notify: []reference{{Type: "service", Title: "absent_svc"}}},
	} {
		t.Run(name, func(t *testing.T) {
			err := validate(&manifest{Class: "meridian_web", Host: fixtures.Host, Resources: []resource{r}})
			var ce *sdk.CompileError
			if !asCompileError(err, &ce) {
				t.Fatalf("error is not a compile error: %v", err)
			}
			if ce.Rule != "dangling-reference" {
				t.Fatalf("rule = %q, want dangling-reference", ce.Rule)
			}
			if !strings.Contains(ce.Msg, "absent_") {
				t.Fatalf("the failure should name the reference that goes nowhere: %q", ce.Msg)
			}
		})
	}
}

func TestAResourceWithNoAttributesIsRefused(t *testing.T) {
	err := validate(&manifest{Class: "meridian_web", Host: fixtures.Host, Resources: []resource{
		{Type: "package", Title: "app", ResourceID: "app"},
	}})
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "empty-resource" {
		t.Fatalf("rule = %q, want empty-resource", ce.Rule)
	}
}

func TestAManifestWithNoNodeIsRefused(t *testing.T) {
	err := validate(&manifest{Class: "meridian_web", Resources: []resource{
		{Type: "package", Title: "app", ResourceID: "app", Attrs: map[string]any{"name": "app"}},
	}})
	var ce *sdk.CompileError
	if !asCompileError(err, &ce) {
		t.Fatalf("error is not a compile error: %v", err)
	}
	if ce.Rule != "missing-host" {
		t.Fatalf("rule = %q, want missing-host", ce.Rule)
	}
}

// TestEveryValidationFailureNamesTheTarget. A host compiling one document for
// several targets reports their failures together, so a message that does not
// say which target refused is a message the operator cannot act on.
func TestEveryValidationFailureNamesTheTarget(t *testing.T) {
	err := emitError(t,
		ir.Resource{ID: "a", Type: "package", Params: map[string]any{"name": "nginx"}},
		ir.Resource{ID: "b", Type: "package", Params: map[string]any{"name": "nginx"}},
	)
	if !strings.HasPrefix(err.Error(), Name+": ") {
		t.Fatalf("failure does not open with the target that raised it: %q", err)
	}
}

// TestNamevarsCoverEveryTypeThatHasOne keeps the derived table honest: a type
// added to mappings with a namevar has to appear here without anyone
// remembering to update a second list.
func TestNamevarsCoverEveryTypeThatHasOne(t *testing.T) {
	for irType, m := range mappings {
		if m.namevar == "" {
			continue
		}
		attr, ok := namevars[m.puppetType]
		if !ok {
			t.Errorf("%s maps to %s, which has a namevar but no entry in the derived table", irType, m.puppetType)
			continue
		}
		if want := m.params[m.namevar]; attr != want {
			t.Errorf("%s identifies its subject by %q, but the table says %q", m.puppetType, want, attr)
		}
	}
}
