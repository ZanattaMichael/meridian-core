package target

import (
	"strings"
	"testing"
)

func TestArtifactPathsAreSorted(t *testing.T) {
	a := Artifact{Files: map[string]string{"roles/web/tasks.yml": "", "playbook.yml": "", "inventory.ini": ""}}
	got := strings.Join(a.Paths(), ",")
	want := "inventory.ini,playbook.yml,roles/web/tasks.yml"
	if got != want {
		t.Fatalf("paths = %s, want %s", got, want)
	}
}

func TestEmptyArtifactHasNoPaths(t *testing.T) {
	if got := (Artifact{}).Paths(); len(got) != 0 {
		t.Fatalf("paths = %v", got)
	}
}

func TestWarningRendersWithAndWithoutAResource(t *testing.T) {
	withResource := Warning{Target: "ansible", Resource: "nginx_svc", Msg: "flattened"}
	if got := withResource.String(); got != "ansible: nginx_svc: flattened" {
		t.Fatalf("warning = %q", got)
	}
	plain := Warning{Target: "ansible", Msg: "flattened"}
	if got := plain.String(); got != "ansible: flattened" {
		t.Fatalf("warning = %q", got)
	}
}
