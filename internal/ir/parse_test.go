package ir

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const validInfrastructure = `apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web-vm
spec:
  target: terraform
  resources:
    - id: web_vm
      type: compute_instance
      params:
        image: ubuntu-22.04
        size: small
  outputs:
    - name: web_vm_ip
      value: "${web_vm.public_ip}"
`

const validResourceSet = `apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web-server-baseline
spec:
  target: ansible
  targetHost:
    from: infrastructure
    ref: web-vm
    output: web_vm_ip
  resources:
    - id: nginx_pkg
      type: package
      state: present
      params:
        name: nginx
    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx/nginx.conf
      dependsOn:
        - resource: nginx_pkg
          ifMissing: error
      notifies:
        - resource: nginx_svc
          on: changed
          action: restart
    - id: nginx_svc
      type: service
      state: running
      dependsOn:
        - resource: nginx_pkg
`

func TestParseValidInfrastructure(t *testing.T) {
	docs, err := Parse("infra.yaml", []byte(validInfrastructure))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if len(docs) != 1 {
		t.Fatalf("got %d documents, want 1", len(docs))
	}
	infra, ok := docs[0].(*Infrastructure)
	if !ok {
		t.Fatalf("got %T, want *Infrastructure", docs[0])
	}
	if infra.Metadata.Name != "web-vm" {
		t.Errorf("metadata.name = %q, want %q", infra.Metadata.Name, "web-vm")
	}
	if infra.Spec.Target != "terraform" {
		t.Errorf("spec.target = %q, want %q", infra.Spec.Target, "terraform")
	}
	if len(infra.Spec.Resources) != 1 {
		t.Fatalf("got %d resources, want 1", len(infra.Spec.Resources))
	}
	r := infra.Spec.Resources[0]
	if r.ID != "web_vm" || r.Type != "compute_instance" {
		t.Errorf("resource = %+v, want id web_vm type compute_instance", r)
	}
	if got := r.Params["image"]; got != "ubuntu-22.04" {
		t.Errorf("params.image = %v, want ubuntu-22.04", got)
	}
	out, ok := infra.Output("web_vm_ip")
	if !ok {
		t.Fatal("output web_vm_ip not found")
	}
	if out.Value != "${web_vm.public_ip}" {
		t.Errorf("output value = %q", out.Value)
	}
}

func TestParseValidResourceSet(t *testing.T) {
	docs, err := Parse("rs.yaml", []byte(validResourceSet))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	rs, ok := docs[0].(*ResourceSet)
	if !ok {
		t.Fatalf("got %T, want *ResourceSet", docs[0])
	}
	if rs.Spec.TargetHost == nil || rs.Spec.TargetHost.Ref != "web-vm" {
		t.Fatalf("targetHost = %+v", rs.Spec.TargetHost)
	}
	if len(rs.Spec.Resources) != 3 {
		t.Fatalf("got %d resources, want 3", len(rs.Spec.Resources))
	}
	for i, r := range rs.Spec.Resources {
		if r.Index != i {
			t.Errorf("resource %q Index = %d, want %d", r.ID, r.Index, i)
		}
		if r.Pos.Line == 0 {
			t.Errorf("resource %q has no source line", r.ID)
		}
	}
	conf := rs.Spec.Resources[1]
	if len(conf.DependsOn) != 1 || conf.DependsOn[0].Resource != "nginx_pkg" {
		t.Errorf("dependsOn = %+v", conf.DependsOn)
	}
	if conf.DependsOn[0].IfMissing != IfMissingError {
		t.Errorf("ifMissing = %q, want %q", conf.DependsOn[0].IfMissing, IfMissingError)
	}
	if len(conf.Notifies) != 1 || conf.Notifies[0].Action != "restart" {
		t.Errorf("notifies = %+v", conf.Notifies)
	}
	if conf.Notifies[0].Pos.Line == 0 {
		t.Error("notify edge has no source line")
	}
}

func TestParseJSONMatchesYAML(t *testing.T) {
	const jsonDoc = `{
  "apiVersion": "meridian/v1",
  "kind": "ResourceSet",
  "metadata": {"name": "web-server-baseline"},
  "spec": {
    "target": "ansible",
    "resources": [
      {"id": "nginx_pkg", "type": "package", "state": "present"},
      {"id": "nginx_svc", "type": "service", "dependsOn": [{"resource": "nginx_pkg"}]}
    ]
  }
}`
	docs, err := Parse("rs.json", []byte(jsonDoc))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	rs := docs[0].(*ResourceSet)
	if len(rs.Spec.Resources) != 2 {
		t.Fatalf("got %d resources, want 2", len(rs.Spec.Resources))
	}
	if rs.Spec.Resources[1].DependsOn[0].Resource != "nginx_pkg" {
		t.Errorf("dependsOn = %+v", rs.Spec.Resources[1].DependsOn)
	}
	if rs.Spec.Resources[1].Index != 1 {
		t.Errorf("Index = %d, want 1", rs.Spec.Resources[1].Index)
	}
	if err := Validate(rs); err != nil {
		t.Errorf("Validate: %v", err)
	}
}

func TestParseJSONArrayIsADocumentStream(t *testing.T) {
	const stream = `[
  {"apiVersion":"meridian/v1","kind":"Infrastructure","metadata":{"name":"a"},
   "spec":{"target":"terraform","resources":[{"id":"x","type":"compute_instance"}],
           "outputs":[{"name":"ip","value":"${x.ip}"}]}},
  {"apiVersion":"meridian/v1","kind":"ResourceSet","metadata":{"name":"b"},
   "spec":{"target":"ansible","targetHost":{"from":"infrastructure","ref":"a","output":"ip"},
           "resources":[{"id":"y","type":"package"}]}}
]`
	docs, err := Parse("bundle.json", []byte(stream))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if len(docs) != 2 {
		t.Fatalf("got %d documents, want 2", len(docs))
	}
	if err := NewBundle(docs...).Validate(); err != nil {
		t.Fatalf("Validate: %v", err)
	}
}

func TestParseMalformedYAMLIsLocatedNotAPanic(t *testing.T) {
	const broken = `apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: broken
 spec:
   target: ansible
`
	defer func() {
		if r := recover(); r != nil {
			t.Fatalf("Parse panicked: %v", r)
		}
	}()
	_, err := Parse("broken.yaml", []byte(broken))
	if err == nil {
		t.Fatal("want an error for malformed YAML")
	}
	var located *Error
	if !asError(err, &located) {
		t.Fatalf("want a located *Error, got %T: %v", err, err)
	}
	if located.Pos.Line == 0 {
		t.Errorf("error has no line: %v", located)
	}
	if !strings.Contains(err.Error(), "broken.yaml") {
		t.Errorf("error does not name the file: %v", err)
	}
}

func TestParseMalformedJSONIsLocated(t *testing.T) {
	const broken = `{
  "apiVersion": "meridian/v1",
  "kind": "ResourceSet",,
}`
	_, err := Parse("broken.json", []byte(broken))
	if err == nil {
		t.Fatal("want an error for malformed JSON")
	}
	var located *Error
	if !asError(err, &located) {
		t.Fatalf("want a located *Error, got %T: %v", err, err)
	}
	if located.Pos.Line != 3 {
		t.Errorf("error line = %d, want 3 (%v)", located.Pos.Line, located)
	}
}

func TestParseUnknownAPIVersionIsExplicit(t *testing.T) {
	for _, tc := range []struct{ name, doc, want string }{
		{
			name: "future version",
			doc:  strings.Replace(validResourceSet, "meridian/v1", "meridian/v9", 1),
			want: "unsupported apiVersion",
		},
		{
			name: "missing",
			doc:  strings.Replace(validResourceSet, "apiVersion: meridian/v1\n", "", 1),
			want: "apiVersion: missing required field",
		},
		{
			name: "unknown kind",
			doc:  strings.Replace(validResourceSet, "kind: ResourceSet", "kind: Playbook", 1),
			want: "unknown kind",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := Parse("doc.yaml", []byte(tc.doc))
			if err == nil {
				t.Fatal("want an error")
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %v, want it to contain %q", err, tc.want)
			}
		})
	}
}

func TestParseUnknownFieldIsAnError(t *testing.T) {
	for _, tc := range []struct{ name, doc string }{
		{"yaml", strings.Replace(validResourceSet, "      type: package", "      type: package\n      typo: nope", 1)},
		{"json", `{"apiVersion":"meridian/v1","kind":"ResourceSet","metadata":{"name":"a"},
		           "spec":{"target":"ansible","resources":[{"id":"x","type":"package","typo":"nope"}]}}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := Parse("doc."+tc.name, []byte(tc.doc))
			if err == nil {
				t.Fatal("want an error: unknown fields must not be silently dropped")
			}
			if !strings.Contains(err.Error(), "typo") {
				t.Errorf("error = %v, want it to name the unknown field", err)
			}
		})
	}
}

func TestParseMultiDocumentYAML(t *testing.T) {
	docs, err := Parse("bundle.yaml", []byte(validInfrastructure+"---\n"+validResourceSet))
	if err != nil {
		t.Fatalf("Parse: %v", err)
	}
	if len(docs) != 2 {
		t.Fatalf("got %d documents, want 2", len(docs))
	}
	if docs[0].KindOf() != KindInfrastructure || docs[1].KindOf() != KindResourceSet {
		t.Fatalf("kinds = %q, %q", docs[0].KindOf(), docs[1].KindOf())
	}
	if docs[1].PositionOf().DocIdx != 1 {
		t.Errorf("second document DocIdx = %d, want 1", docs[1].PositionOf().DocIdx)
	}
	// The second document starts after the first, so its line must be larger.
	if docs[1].PositionOf().Line <= docs[0].PositionOf().Line {
		t.Errorf("document lines = %d, %d", docs[0].PositionOf().Line, docs[1].PositionOf().Line)
	}
}

func TestParseFileReadsFromDisk(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "infra.yaml")
	if err := os.WriteFile(path, []byte(validInfrastructure), 0o600); err != nil {
		t.Fatal(err)
	}
	docs, err := ParseFile(path)
	if err != nil {
		t.Fatalf("ParseFile: %v", err)
	}
	if docs[0].NameOf() != "web-vm" {
		t.Errorf("name = %q", docs[0].NameOf())
	}
	if _, err := ParseFile(filepath.Join(dir, "absent.yaml")); err == nil {
		t.Error("want an error for a missing file")
	}
}

func TestParseIsDeterministic(t *testing.T) {
	first, err := Parse("bundle.yaml", []byte(validInfrastructure+"---\n"+validResourceSet))
	if err != nil {
		t.Fatal(err)
	}
	want := renderDocs(first)
	for i := 0; i < 50; i++ {
		docs, err := Parse("bundle.yaml", []byte(validInfrastructure+"---\n"+validResourceSet))
		if err != nil {
			t.Fatal(err)
		}
		if got := renderDocs(docs); got != want {
			t.Fatalf("run %d differs:\n got %s\nwant %s", i, got, want)
		}
	}
}

func renderDocs(docs []Document) string {
	var b strings.Builder
	for _, d := range docs {
		b.WriteString(string(d.KindOf()) + "/" + d.NameOf() + ":")
		for _, r := range d.ResourcesOf() {
			b.WriteString(" " + r.ID)
		}
		b.WriteString(";")
	}
	return b.String()
}
