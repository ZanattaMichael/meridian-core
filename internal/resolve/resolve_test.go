package resolve

import (
	"strings"
	"testing"
	"testing/fstest"
)

// standardHierarchy is the design plan's example: lowest priority first, so
// node overrides role overrides environment overrides common.
func standardHierarchy() *Hierarchy {
	return &Hierarchy{
		Paths: []string{
			"common.yaml",
			"environment/%{env}.yaml",
			"role/%{role}.yaml",
			"node/%{node}.yaml",
		},
	}
}

func standardScope() Scope {
	return Scope{"env": "prod", "role": "webserver", "node": "web01"}
}

func TestExpandPathsInterpolates(t *testing.T) {
	paths, err := standardHierarchy().ExpandPaths(standardScope())
	if err != nil {
		t.Fatalf("ExpandPaths: %v", err)
	}
	want := []string{"common.yaml", "environment/prod.yaml", "role/webserver.yaml", "node/web01.yaml"}
	if len(paths) != len(want) {
		t.Fatalf("got %v, want %v", paths, want)
	}
	for i := range want {
		if paths[i] != want[i] {
			t.Errorf("paths[%d] = %q, want %q", i, paths[i], want[i])
		}
	}
}

func TestExpandPathsRejectsUndefinedVariable(t *testing.T) {
	_, err := standardHierarchy().ExpandPaths(Scope{"env": "prod"})
	if err == nil || !strings.Contains(err.Error(), "undefined variable %{role}") {
		t.Fatalf("error = %v, want an undefined-variable error", err)
	}
}

func TestExpandPathsDetectsCircularReference(t *testing.T) {
	h := &Hierarchy{Paths: []string{"%{a}.yaml"}}
	_, err := h.ExpandPaths(Scope{"a": "%{b}", "b": "%{a}"})
	if err == nil {
		t.Fatal("want an error, not infinite recursion")
	}
	if !strings.Contains(err.Error(), "circular hierarchy reference") {
		t.Fatalf("error = %v, want a circular-reference error", err)
	}
	if !strings.Contains(err.Error(), "a -> b -> a") {
		t.Errorf("error = %v, want it to show the full chain", err)
	}
}

func TestExpandPathsDetectsSelfReference(t *testing.T) {
	h := &Hierarchy{Paths: []string{"%{a}.yaml"}}
	_, err := h.ExpandPaths(Scope{"a": "%{a}"})
	if err == nil || !strings.Contains(err.Error(), "circular hierarchy reference") {
		t.Fatalf("error = %v, want a circular-reference error", err)
	}
}

func TestExpandPathsRejectsDuplicateLayer(t *testing.T) {
	h := &Hierarchy{Paths: []string{"role/%{role}.yaml", "node/%{node}.yaml"}}
	_, err := h.ExpandPaths(Scope{"role": "x", "node": "../role/x"})
	if err == nil || !strings.Contains(err.Error(), "cannot appear twice") {
		t.Fatalf("error = %v, want a duplicate-layer error", err)
	}
}

func TestExpandPathsRejectsMalformedInterpolation(t *testing.T) {
	for _, tc := range []struct{ path, want string }{
		{"role/%{role.yaml", "unterminated"},
		{"role/%{}.yaml", "empty interpolation"},
	} {
		h := &Hierarchy{Paths: []string{tc.path}}
		_, err := h.ExpandPaths(Scope{"role": "x"})
		if err == nil || !strings.Contains(err.Error(), tc.want) {
			t.Errorf("path %q: error = %v, want it to contain %q", tc.path, err, tc.want)
		}
	}
}

func TestFirstFoundTakesHighestPriorityLayer(t *testing.T) {
	loader := MapLoader{
		"common.yaml":           {"ntp_server": "pool.ntp.org", "only_common": "kept"},
		"environment/prod.yaml": {"ntp_server": "prod.ntp.internal"},
		"role/webserver.yaml":   {"ntp_server": "role.ntp.internal"},
		"node/web01.yaml":       {"ntp_server": "web01.ntp.internal"},
	}
	res, err := Resolve(standardHierarchy(), standardScope(), loader)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if got := res.Data["ntp_server"]; got != "web01.ntp.internal" {
		t.Errorf("ntp_server = %v, want the node layer's value", got)
	}
	if got := res.Data["only_common"]; got != "kept" {
		t.Errorf("only_common = %v, want it preserved from the common layer", got)
	}
}

func TestPrecedenceOrderNodeRoleEnvironmentCommon(t *testing.T) {
	full := MapLoader{
		"common.yaml":           {"who": "common"},
		"environment/prod.yaml": {"who": "environment"},
		"role/webserver.yaml":   {"who": "role"},
		"node/web01.yaml":       {"who": "node"},
	}
	// Drop the highest layer each time and assert the next one down wins.
	for _, tc := range []struct{ drop, want string }{
		{"", "node"},
		{"node/web01.yaml", "role"},
		{"role/webserver.yaml", "environment"},
		{"environment/prod.yaml", "common"},
	} {
		if tc.drop != "" {
			delete(full, tc.drop)
		}
		res, err := Resolve(standardHierarchy(), standardScope(), full)
		if err != nil {
			t.Fatalf("Resolve after dropping %q: %v", tc.drop, err)
		}
		if got := res.Data["who"]; got != tc.want {
			t.Errorf("after dropping %q: who = %v, want %q", tc.drop, got, tc.want)
		}
	}
}

func TestDeepMergePreservesSiblingKeys(t *testing.T) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{"nginx": {Strategy: DeepMerge}}
	loader := MapLoader{
		"common.yaml": {"nginx": map[string]any{
			"worker_processes": 2,
			"limits":           map[string]any{"nofile": 1024, "nproc": 512},
			"listen":           80,
		}},
		"node/web01.yaml": {"nginx": map[string]any{
			"limits": map[string]any{"nofile": 65535},
		}},
	}
	res, err := Resolve(h, standardScope(), loader)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	nginx, ok := res.Data["nginx"].(map[string]any)
	if !ok {
		t.Fatalf("nginx = %T, want a map", res.Data["nginx"])
	}
	if nginx["worker_processes"] != 2 || nginx["listen"] != 80 {
		t.Errorf("top-level siblings clobbered: %v", nginx)
	}
	limits, ok := nginx["limits"].(map[string]any)
	if !ok {
		t.Fatalf("limits = %T, want a map", nginx["limits"])
	}
	if limits["nofile"] != 65535 {
		t.Errorf("nofile = %v, want the node layer's 65535", limits["nofile"])
	}
	if limits["nproc"] != 512 {
		t.Errorf("nproc = %v, want the common layer's 512 to survive", limits["nproc"])
	}
}

func TestDeepMergeDoesNotMutateLayerData(t *testing.T) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{"cfg": {Strategy: DeepMerge}}
	base := map[string]any{"a": map[string]any{"x": 1}}
	loader := MapLoader{
		"common.yaml":     {"cfg": base},
		"node/web01.yaml": {"cfg": map[string]any{"a": map[string]any{"y": 2}}},
	}
	if _, err := Resolve(h, standardScope(), loader); err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	inner := base["a"].(map[string]any)
	if _, leaked := inner["y"]; leaked {
		t.Error("merge wrote back into the source layer's data")
	}
}

func TestUniqueArrayMergePrimitives(t *testing.T) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{"packages": {Strategy: UniqueArrayMerge}}
	loader := MapLoader{
		"common.yaml":           {"packages": []any{"curl", "vim"}},
		"environment/prod.yaml": {"packages": []any{"vim", "htop"}},
		"node/web01.yaml":       {"packages": []any{"curl", "nginx"}},
	}
	res, err := Resolve(h, standardScope(), loader)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	want := []string{"curl", "vim", "htop", "nginx"}
	got, ok := res.Data["packages"].([]any)
	if !ok {
		t.Fatalf("packages = %T, want a slice", res.Data["packages"])
	}
	if len(got) != len(want) {
		t.Fatalf("packages = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("packages[%d] = %v, want %q", i, got[i], want[i])
		}
	}
}

func TestUniqueArrayMergeObjectsWithDedupKey(t *testing.T) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{"users": {Strategy: UniqueArrayMerge, DedupKey: "name"}}
	loader := MapLoader{
		"common.yaml": {"users": []any{
			map[string]any{"name": "deploy", "shell": "/bin/sh"},
			map[string]any{"name": "ops", "shell": "/bin/bash"},
		}},
		"node/web01.yaml": {"users": []any{
			map[string]any{"name": "deploy", "shell": "/bin/bash"},
			map[string]any{"name": "app", "shell": "/bin/false"},
		}},
	}
	res, err := Resolve(h, standardScope(), loader)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	users := res.Data["users"].([]any)
	if len(users) != 3 {
		t.Fatalf("got %d users, want 3: %v", len(users), users)
	}
	first := users[0].(map[string]any)
	// The dedup key keeps the FIRST occurrence, which is the lowest-priority
	// layer's. This is the documented behaviour and the easy silent-bug spot:
	// authors who want the node layer to win should use deep-merge instead.
	if first["name"] != "deploy" || first["shell"] != "/bin/sh" {
		t.Errorf("first user = %v, want the common layer's deploy entry kept", first)
	}
}

func TestUniqueArrayMergeObjectsWithoutDedupKey(t *testing.T) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{"users": {Strategy: UniqueArrayMerge}}
	loader := MapLoader{
		"common.yaml": {"users": []any{
			map[string]any{"name": "deploy", "shell": "/bin/sh"},
		}},
		"node/web01.yaml": {"users": []any{
			// Same field values in a different declaration order: the whole
			// canonical value is the identity, so this is a duplicate.
			map[string]any{"shell": "/bin/sh", "name": "deploy"},
			map[string]any{"name": "deploy", "shell": "/bin/bash"},
		}},
	}
	res, err := Resolve(h, standardScope(), loader)
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if users := res.Data["users"].([]any); len(users) != 2 {
		t.Fatalf("got %d users, want 2: %v", len(users), users)
	}
}

func TestUniqueArrayMergeReportsMissingDedupKey(t *testing.T) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{"users": {Strategy: UniqueArrayMerge, DedupKey: "name"}}
	loader := MapLoader{"common.yaml": {"users": []any{map[string]any{"uid": 1}}}}
	_, err := Resolve(h, standardScope(), loader)
	if err == nil || !strings.Contains(err.Error(), `no dedup key "name"`) {
		t.Fatalf("error = %v, want a missing-dedup-key error", err)
	}
}

func TestUniqueArrayMergeRejectsNonArrayLayer(t *testing.T) {
	h := standardHierarchy()
	h.Keys = map[string]KeyRule{"packages": {Strategy: UniqueArrayMerge}}
	loader := MapLoader{"common.yaml": {"packages": "curl"}}
	_, err := Resolve(h, standardScope(), loader)
	if err == nil || !strings.Contains(err.Error(), "requires every layer to provide an array") {
		t.Fatalf("error = %v, want a type error", err)
	}
}

func TestMissingLayerLenientAndStrict(t *testing.T) {
	// Only the common layer exists; the other three are absent.
	loader := MapLoader{"common.yaml": {"who": "common"}}

	lenient := standardHierarchy()
	lenient.Strictness = Lenient
	res, err := Resolve(lenient, standardScope(), loader)
	if err != nil {
		t.Fatalf("lenient Resolve: %v", err)
	}
	if res.Data["who"] != "common" {
		t.Errorf("lenient who = %v, want common", res.Data["who"])
	}
	var absent int
	for _, l := range res.Layers {
		if !l.Found {
			absent++
		}
	}
	if absent != 3 {
		t.Errorf("got %d absent layers recorded, want 3", absent)
	}

	strict := standardHierarchy()
	strict.Strictness = Strict
	_, err = Resolve(strict, standardScope(), loader)
	if err == nil || !strings.Contains(err.Error(), "hierarchy layer not found") {
		t.Fatalf("strict error = %v, want a missing-layer error", err)
	}
	if !strings.Contains(err.Error(), "environment/prod.yaml") {
		t.Errorf("strict error = %v, want it to name the first missing layer", err)
	}
}

func TestDefaultStrictnessIsLenient(t *testing.T) {
	h := standardHierarchy()
	if h.Strictness != "" {
		t.Fatal("test assumes an unset strictness")
	}
	if _, err := Resolve(h, standardScope(), MapLoader{"common.yaml": {"a": 1}}); err != nil {
		t.Fatalf("Resolve: %v", err)
	}
}

func TestHierarchyValidateRejectsBadConfiguration(t *testing.T) {
	for _, tc := range []struct {
		name string
		h    *Hierarchy
		want string
	}{
		{"no layers", &Hierarchy{}, "no layers declared"},
		{"bad strictness", &Hierarchy{Paths: []string{"a.yaml"}, Strictness: "paranoid"}, "unknown strictness"},
		{"bad default strategy", &Hierarchy{Paths: []string{"a.yaml"}, Default: KeyRule{Strategy: "smoosh"}}, "unknown merge strategy"},
		{"bad key strategy", &Hierarchy{Paths: []string{"a.yaml"}, Keys: map[string]KeyRule{"k": {Strategy: "smoosh"}}}, "keys.k"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if err := tc.h.Validate(); err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %v, want it to contain %q", err, tc.want)
			}
		})
	}
}

func TestFSLoaderReadsYAML(t *testing.T) {
	fsys := fstest.MapFS{
		"common.yaml":     {Data: []byte("ntp_server: pool.ntp.org\npackages: [curl]\n")},
		"node/web01.yaml": {Data: []byte("ntp_server: web01.ntp.internal\n")},
	}
	h := standardHierarchy()
	res, err := Resolve(h, standardScope(), FSLoader{FS: fsys})
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if res.Data["ntp_server"] != "web01.ntp.internal" {
		t.Errorf("ntp_server = %v", res.Data["ntp_server"])
	}
}

func TestFSLoaderReportsMalformedYAML(t *testing.T) {
	fsys := fstest.MapFS{"common.yaml": {Data: []byte("a: [1, 2\n")}}
	_, err := Resolve(&Hierarchy{Paths: []string{"common.yaml"}}, nil, FSLoader{FS: fsys})
	if err == nil || !strings.Contains(err.Error(), "common.yaml") {
		t.Fatalf("error = %v, want it to name the offending layer", err)
	}
}
