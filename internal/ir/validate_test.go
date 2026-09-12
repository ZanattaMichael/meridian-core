package ir

import (
	"strings"
	"testing"
)

func parseOne(t *testing.T, doc string) Document {
	t.Helper()
	docs, err := Parse("doc.yaml", []byte(doc))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if len(docs) != 1 {
		t.Fatalf("got %d documents, want 1", len(docs))
	}
	return docs[0]
}

func TestValidateAcceptsGoodDocuments(t *testing.T) {
	for _, doc := range []string{validInfrastructure, validResourceSet} {
		if err := Validate(parseOne(t, doc)); err != nil {
			t.Errorf("Validate: %v", err)
		}
	}
}

func TestValidateRejectsDuplicateResourceID(t *testing.T) {
	const doc = `apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: dupes}
spec:
  target: ansible
  resources:
    - id: nginx
      type: package
    - id: nginx
      type: service
`
	err := Validate(parseOne(t, doc))
	if err == nil {
		t.Fatal("want an error for a duplicate id")
	}
	if !strings.Contains(err.Error(), `duplicate resource id "nginx"`) {
		t.Errorf("error = %v", err)
	}
	// The message must point at the first declaration too, so the author can
	// find both halves of the collision.
	if !strings.Contains(err.Error(), "first declared at") {
		t.Errorf("error does not locate the first declaration: %v", err)
	}
}

func TestValidateRejectsDanglingEdges(t *testing.T) {
	const doc = `apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: dangling}
spec:
  target: ansible
  resources:
    - id: nginx_conf
      type: file
      dependsOn:
        - resource: absent_pkg
      notifies:
        - resource: absent_svc
          action: restart
`
	err := Validate(parseOne(t, doc))
	if err == nil {
		t.Fatal("want an error for dangling edges")
	}
	for _, want := range []string{
		`depends on unknown resource "absent_pkg"`,
		`notifies unknown resource "absent_svc"`,
	} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("error = %v, want it to contain %q", err, want)
		}
	}
}

func TestValidateRejectsSelfReference(t *testing.T) {
	const doc = `apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: selfref}
spec:
  target: ansible
  resources:
    - id: nginx
      type: service
      dependsOn:
        - resource: nginx
`
	err := Validate(parseOne(t, doc))
	if err == nil || !strings.Contains(err.Error(), "cannot depend on itself") {
		t.Fatalf("error = %v, want a self-dependency rejection", err)
	}
}

func TestValidateRejectsMissingRequiredFields(t *testing.T) {
	for _, tc := range []struct{ name, doc, want string }{
		{
			name: "no metadata.name",
			doc:  "apiVersion: meridian/v1\nkind: ResourceSet\nspec:\n  target: ansible\n  resources:\n    - {id: a, type: package}\n",
			want: "metadata.name: missing required field",
		},
		{
			name: "no spec.target",
			doc:  "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  resources:\n    - {id: a, type: package}\n",
			want: "spec.target: missing required field",
		},
		{
			name: "no resources",
			doc:  "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  target: ansible\n  resources: []\n",
			want: "at least one resource is required",
		},
		{
			name: "resource without id",
			doc:  "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  target: ansible\n  resources:\n    - {type: package}\n",
			want: "spec.resources[0].id: missing required field",
		},
		{
			name: "resource without type",
			doc:  "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  target: ansible\n  resources:\n    - {id: a}\n",
			want: "spec.resources[0].type: missing required field",
		},
		{
			name: "unknown ifMissing policy",
			doc: "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  target: ansible\n  resources:\n" +
				"    - {id: a, type: package}\n    - {id: b, type: service, dependsOn: [{resource: a, ifMissing: shrug}]}\n",
			want: "unknown policy",
		},
		{
			name: "notify without action",
			doc: "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  target: ansible\n  resources:\n" +
				"    - {id: a, type: package}\n    - {id: b, type: service, notifies: [{resource: a}]}\n",
			want: "notifies[0].action: missing required field",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := Validate(parseOne(t, tc.doc))
			if err == nil {
				t.Fatal("want an error")
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %v, want it to contain %q", err, tc.want)
			}
		})
	}
}

func TestValidateReportsEveryProblemAtOnce(t *testing.T) {
	const doc = `apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: messy}
spec:
  target: ansible
  resources:
    - id: a
      type: package
    - id: a
      type: service
      dependsOn:
        - resource: ghost
`
	err := Validate(parseOne(t, doc))
	list, ok := err.(ErrorList)
	if !ok {
		t.Fatalf("got %T, want ErrorList: %v", err, err)
	}
	if len(list) != 2 {
		t.Fatalf("got %d diagnostics, want 2: %v", len(list), err)
	}
}

func TestBundleResolvesTargetHostAcrossDocuments(t *testing.T) {
	docs, err := Parse("bundle.yaml", []byte(validInfrastructure+"---\n"+validResourceSet))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	b := NewBundle(docs...)
	if err := b.Validate(); err != nil {
		t.Fatalf("Validate: %v", err)
	}
	rs, ok := b.ResourceSet("web-server-baseline")
	if !ok {
		t.Fatal("ResourceSet not found in bundle")
	}
	infra, out, err := b.ResolveTargetHost(rs)
	if err != nil {
		t.Fatalf("ResolveTargetHost: %v", err)
	}
	if infra.Metadata.Name != "web-vm" {
		t.Errorf("resolved infrastructure = %q, want web-vm", infra.Metadata.Name)
	}
	if out.Name != "web_vm_ip" || out.Value != "${web_vm.public_ip}" {
		t.Errorf("resolved output = %+v", out)
	}
}

func TestBundleRejectsUnresolvableTargetHost(t *testing.T) {
	for _, tc := range []struct{ name, mutation, want string }{
		{"unknown ref", "ref: web-vm", "no Infrastructure document named"},
		{"unknown output", "output: web_vm_ip", "declares no output"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			broken := strings.Replace(validResourceSet, tc.mutation, strings.Split(tc.mutation, ":")[0]+": nope", 1)
			docs, err := Parse("bundle.yaml", []byte(validInfrastructure+"---\n"+broken))
			if err != nil {
				t.Fatalf("Parse: %v", err)
			}
			err = NewBundle(docs...).Validate()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want it to contain %q", err, tc.want)
			}
		})
	}
}

func TestBundleValidatesTargetHostFromInline(t *testing.T) {
	const doc = `apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: inline}
spec:
  target: ansible
  targetHost:
    from: inline
  resources:
    - {id: a, type: package}
`
	err := NewBundle(parseOne(t, doc)).Validate()
	if err == nil || !strings.Contains(err.Error(), "targetHost.host: missing required field") {
		t.Fatalf("error = %v, want inline targetHost to require a host", err)
	}
}

func TestBundleRejectsDuplicateDocumentNames(t *testing.T) {
	docs, err := Parse("bundle.yaml", []byte(validResourceSet+"---\n"+validResourceSet))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	err = NewBundle(docs...).Validate()
	if err == nil || !strings.Contains(err.Error(), "duplicate ResourceSet document name") {
		t.Fatalf("error = %v, want a duplicate-name rejection", err)
	}
}
